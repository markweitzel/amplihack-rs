//! FIX 2 — E.164-validated group membership parsing (fail-closed, RED until
//! implemented).
//!
//! `transport::parse_group_members` reads a signal-cli `listGroups` result and
//! returns the target group's members, validating **every** number as E.164
//! (`+` then 1..=15 ASCII digits) by reusing the crate's single `validate_e164`
//! predicate. It is **fail-closed**: the first empty or non-conforming member
//! rejects the whole parse with [`WireError::Membership`], and the group being
//! absent from the result is likewise a rejection (we cannot verify membership).
//!
//! Security: the error must **never leak member phone numbers** — the
//! `Membership` variant carries no number, so its `Display` is a fixed string.
#![cfg(feature = "signal")]

use amplihack_signal::transport::{WireError, parse_group_members};
use serde_json::{Value, json};

/// Build a `listGroups`-shaped result containing one group with `members`
/// (each rendered as `{"number": ...}`), plus an unrelated decoy group.
fn list_groups_result(group_id: &str, members: &[&str]) -> Value {
    let member_objs: Vec<Value> = members.iter().map(|n| json!({ "number": n })).collect();
    json!([
        { "id": "grp-OTHER==", "name": "decoy", "members": [{ "number": "+15550000000" }] },
        { "id": group_id, "name": "session", "members": member_objs }
    ])
}

#[test]
fn parses_valid_e164_membership_for_the_target_group() {
    let result = list_groups_result("grp-abc123==", &["+15551230001", "+15551230002"]);
    let members = parse_group_members(&result, "grp-abc123==").expect("valid membership parses");
    assert_eq!(
        members,
        vec!["+15551230001".to_string(), "+15551230002".to_string()],
        "only the target group's members are returned"
    );
}

#[test]
fn empty_member_number_rejects_whole_parse() {
    // A single empty number fails the entire parse (fail-closed): we must not
    // return a partially-validated membership set.
    let result = list_groups_result("grp-abc123==", &["+15551230001", ""]);
    let err = parse_group_members(&result, "grp-abc123==").unwrap_err();
    assert!(
        matches!(err, WireError::Membership),
        "empty member number must reject with WireError::Membership, got {err:?}"
    );
}

#[test]
fn malformed_member_number_rejects_whole_parse() {
    // Non-digit characters after '+' are not E.164 and reject the whole parse.
    let result = list_groups_result("grp-abc123==", &["+1555ABC0001", "+15551230002"]);
    let err = parse_group_members(&result, "grp-abc123==").unwrap_err();
    assert!(
        matches!(err, WireError::Membership),
        "malformed member number must reject with WireError::Membership, got {err:?}"
    );
}

#[test]
fn overlong_member_number_rejects_whole_parse() {
    // 16 digits exceeds the E.164 1..=15 bound.
    let result = list_groups_result("grp-abc123==", &["+1234567890123456"]);
    let err = parse_group_members(&result, "grp-abc123==").unwrap_err();
    assert!(matches!(err, WireError::Membership), "got {err:?}");
}

#[test]
fn missing_number_prefix_rejects_whole_parse() {
    // No leading '+' is not E.164.
    let result = list_groups_result("grp-abc123==", &["15551230001"]);
    let err = parse_group_members(&result, "grp-abc123==").unwrap_err();
    assert!(matches!(err, WireError::Membership), "got {err:?}");
}

#[test]
fn absent_target_group_is_fail_closed() {
    // The requested group is not present in the daemon's listing: we cannot
    // verify membership, so reject rather than treat it as "no members".
    let result = list_groups_result("grp-SOMETHING-ELSE==", &["+15551230001"]);
    let err = parse_group_members(&result, "grp-abc123==").unwrap_err();
    assert!(
        matches!(err, WireError::Membership),
        "an absent target group must fail closed, got {err:?}"
    );
}

#[test]
fn parse_failure_message_does_not_leak_member_numbers() {
    // A malformed number embedding a recognizable token must not appear in the
    // error's Display output — the Membership variant is number-free.
    let secret = "+1555LEAKME9999";
    let result = list_groups_result("grp-abc123==", &[secret, "+15551230002"]);
    let err = parse_group_members(&result, "grp-abc123==").unwrap_err();
    let rendered = err.to_string();
    assert!(
        !rendered.contains("LEAKME") && !rendered.contains(secret),
        "membership error must not leak member numbers; got {rendered:?}"
    );
}
