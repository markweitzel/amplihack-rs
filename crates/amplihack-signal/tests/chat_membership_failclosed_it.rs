//! TDD contract — F3: fail-closed group-membership parsing.
//!
//! Written **first** (Step 7 TDD). Relaying agent output leaks it to every
//! group member, so membership verification must be *positive-only*: any
//! member the chat cannot fully account for withholds the relay.
//!
//! The current `parse_group_members` uses a `filter_map` that **silently
//! drops** a member lacking an E.164 `number` and returns the surviving subset
//! as `Ok`. That is fail-*open*: an unaccounted-for member (e.g. one whose
//! `number` field is absent) vanishes from the verified set, so the mismatch
//! check can spuriously pass. After F3, a member missing the `number` field is
//! a parse failure (`Err(WireError::Membership)`), which the caller maps to
//! `group_members == None` → `classify(_, None)` → `Membership::Unverified`,
//! and the relay is withheld.
//!
//! Run: `cargo test -p amplihack-signal --features signal --test
//! chat_membership_failclosed_it`.
#![cfg(feature = "signal")]

use amplihack_signal::chat::membership::{Membership, classify};
use amplihack_signal::transport::{WireError, parse_group_members};
use serde_json::json;

const GROUP: &str = "group.abcdef==";

/// A well-formed `listGroups` result where every member carries a `number`.
fn all_numbered() -> serde_json::Value {
    json!([{
        "id": GROUP,
        "members": [
            { "number": "+12065551234" },
            { "number": "+12065559999" },
        ]
    }])
}

/// The same group, but one member is missing the `number` field entirely
/// (e.g. a member known only by ACI/UUID). Under fail-closed parsing this must
/// NOT be silently dropped.
fn one_member_missing_number() -> serde_json::Value {
    json!([{
        "id": GROUP,
        "members": [
            { "number": "+12065551234" },
            { "uuid": "8d9f0e2a-0000-4000-8000-000000000000" },
        ]
    }])
}

#[test]
fn well_formed_members_parse_to_their_numbers() {
    // Sanity: the happy path is unchanged.
    let members =
        parse_group_members(&all_numbered(), GROUP).expect("fully-numbered members must parse");
    assert_eq!(members, vec!["+12065551234", "+12065559999"]);
}

#[test]
fn member_missing_number_is_a_parse_failure() {
    // Fail-closed: a member without a string `number` must make the whole
    // parse fail — never silently drop the member and return the subset.
    let err = parse_group_members(&one_member_missing_number(), GROUP)
        .expect_err("a member missing `number` must fail closed, not be dropped");
    assert!(
        matches!(err, WireError::Membership(_)),
        "expected WireError::Membership, got {err:?}"
    );
}

#[test]
fn member_missing_number_classifies_as_unverified() {
    // End-to-end fail-closed authorization: the caller turns the parse failure
    // into `None` (no positively-known member set), which classify() treats as
    // Unverified, so the relay is withheld.
    let expected = vec!["+12065551234".to_string(), "+12065559999".to_string()];
    let actual: Option<Vec<String>> = parse_group_members(&one_member_missing_number(), GROUP).ok();
    let membership = classify(&expected, actual.as_deref());

    assert!(
        matches!(membership, Membership::Unverified(_)),
        "a member missing its E.164 number must yield Unverified, got {membership:?}"
    );
    assert!(
        !membership.may_relay(),
        "relay must be withheld when membership is unverified"
    );
}

#[test]
fn member_with_non_string_number_is_a_parse_failure() {
    // A `number` present but not a string is equally unaccountable → fail closed.
    let value = json!([{
        "id": GROUP,
        "members": [
            { "number": "+12065551234" },
            { "number": 12065559999_i64 },
        ]
    }]);
    let err =
        parse_group_members(&value, GROUP).expect_err("a non-string `number` must fail closed");
    assert!(matches!(err, WireError::Membership(_)), "got {err:?}");
}

// ---------------------------------------------------------------------------
// FIX 2 (F5) — E.164 *format* validation of every member number.
//
// The current parser accepts ANY non-empty string in a member's `number`
// field, so a blank or garbage value survives into the "verified" set and the
// exact-set match can be gamed. After F2 every member number must satisfy the
// same E.164 predicate used elsewhere in the crate (`+` then 1..=15 ASCII
// digits); the FIRST empty or malformed number fails the whole parse
// (fail-closed), and the error still names only the defect, never a number.
// ---------------------------------------------------------------------------

/// A group where one member's `number` is present but empty.
fn one_member_empty_number() -> serde_json::Value {
    json!([{
        "id": GROUP,
        "members": [
            { "number": "+12065551234" },
            { "number": "" },
        ]
    }])
}

/// A group where one member's `number` is a string but not a valid E.164
/// number (no `+`, or non-digit body, or too many digits).
fn one_member_malformed_number() -> serde_json::Value {
    json!([{
        "id": GROUP,
        "members": [
            { "number": "+12065551234" },
            { "number": "12065559999" }, // missing leading '+'
        ]
    }])
}

#[test]
fn member_with_empty_number_is_a_parse_failure() {
    // Fail-closed: an empty `number` is not a positively-known member.
    let err = parse_group_members(&one_member_empty_number(), GROUP)
        .expect_err("an empty `number` must fail closed, not be accepted");
    assert!(
        matches!(err, WireError::Membership(_)),
        "expected WireError::Membership for an empty number, got {err:?}"
    );
}

#[test]
fn member_with_malformed_e164_is_a_parse_failure() {
    // A syntactically invalid E.164 number (missing `+`) is unaccountable.
    let err = parse_group_members(&one_member_malformed_number(), GROUP)
        .expect_err("a malformed E.164 `number` must fail closed");
    assert!(
        matches!(err, WireError::Membership(_)),
        "expected WireError::Membership for a malformed number, got {err:?}"
    );
}

#[test]
fn member_with_non_ascii_or_overlong_e164_is_a_parse_failure() {
    // `+` followed by more than 15 digits, and `+` followed by non-digits, are
    // both outside E.164 and must fail closed.
    let overlong = json!([{
        "id": GROUP,
        "members": [
            { "number": "+12065551234" },
            { "number": "+1234567890123456" }, // 16 digits > E.164 max of 15
        ]
    }]);
    let err = parse_group_members(&overlong, GROUP)
        .expect_err("an over-length E.164 number must fail closed");
    assert!(matches!(err, WireError::Membership(_)), "got {err:?}");

    let non_digit = json!([{
        "id": GROUP,
        "members": [
            { "number": "+12065551234" },
            { "number": "+1206555abcd" }, // non-digit body
        ]
    }]);
    let err = parse_group_members(&non_digit, GROUP)
        .expect_err("a non-digit E.164 body must fail closed");
    assert!(matches!(err, WireError::Membership(_)), "got {err:?}");
}

#[test]
fn member_empty_number_classifies_as_unverified() {
    // End-to-end fail-closed: an invalid member number yields no positively
    // known set → classify() → Unverified → relay withheld.
    let expected = vec!["+12065551234".to_string(), "+12065559999".to_string()];
    let actual: Option<Vec<String>> = parse_group_members(&one_member_empty_number(), GROUP).ok();
    let membership = classify(&expected, actual.as_deref());
    assert!(
        matches!(membership, Membership::Unverified(_)),
        "an empty member number must yield Unverified, got {membership:?}"
    );
    assert!(!membership.may_relay(), "relay must be withheld");
}

#[test]
fn valid_e164_members_still_parse_after_format_tightening() {
    // Guard against over-tightening: legitimate, well-formed E.164 members must
    // continue to parse unchanged.
    let members =
        parse_group_members(&all_numbered(), GROUP).expect("valid E.164 members must still parse");
    assert_eq!(members, vec!["+12065551234", "+12065559999"]);
}

#[test]
fn malformed_number_parse_error_does_not_leak_member_numbers() {
    // PII discipline holds for the new format-validation path too.
    let err = parse_group_members(&one_member_malformed_number(), GROUP).unwrap_err();
    let WireError::Membership(msg) = err else {
        panic!("expected WireError::Membership");
    };
    assert!(
        !msg.contains("12065559999") && !msg.contains("+12065551234"),
        "membership format error must not leak a member number: {msg:?}"
    );
}

#[test]
fn parse_failure_message_does_not_leak_member_numbers() {
    // PII discipline: the error surfaced to logs/audit must reference the
    // defect, not embed any member phone number.
    let err = parse_group_members(&one_member_missing_number(), GROUP).unwrap_err();
    let WireError::Membership(msg) = err else {
        panic!("expected WireError::Membership");
    };
    assert!(
        !msg.contains("+12065551234"),
        "membership parse error must not leak a member number: {msg:?}"
    );
}
