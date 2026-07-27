//! FIX 2 (RED) — E.164-validated group-member parsing, fail-closed and PII-free.
//!
//! Contract under test (currently UNIMPLEMENTED — expected to FAIL until FIX 2
//! lands). A new pure wire helper
//!
//!   `amplihack_signal::transport::parse_group_members(&serde_json::Value)
//!        -> Result<Vec<String>, WireError>`
//!
//! extracts the E.164 member numbers from a signal-cli group-info result and
//! validates each with the in-crate `config::resolver::validate_e164` predicate
//! (`+` followed by 1..=15 ASCII digits). It is **fail-closed**:
//!
//!   * an empty member set is rejected,
//!   * the FIRST empty or non-conforming number rejects the WHOLE parse,
//!   * the failure surfaces as the new `WireError::Membership` variant, and
//!   * the error message MUST NOT leak any member number (no PII in logs).
//!
//! Validation stays in-crate on purpose: importing the validator from
//! `amplihack-cli` would invert the dependency direction (cli -> signal only)
//! and create a cycle, so `validate_e164` is promoted to `pub(crate)` and
//! reused here rather than duplicated.
//!
//! Member JSON shape (signal-cli `listGroups`/group-info): each member is an
//! object carrying its `number`, e.g.
//!   { "members": [ { "number": "+15551230001" }, { "number": "+15551230002" } ] }
#![cfg(feature = "signal")]

use amplihack_signal::transport::{WireError, parse_group_members};
use serde_json::json;

#[test]
fn valid_members_parse_to_e164_numbers_in_order() {
    let group = json!({
        "members": [
            { "number": "+15551230001" },
            { "number": "+15551230002" },
            { "number": "+447700900123" }
        ]
    });
    let members = parse_group_members(&group).expect("all-valid roster parses");
    assert_eq!(
        members,
        vec![
            "+15551230001".to_string(),
            "+15551230002".to_string(),
            "+447700900123".to_string(),
        ]
    );
}

#[test]
fn empty_member_set_is_rejected_fail_closed() {
    let group = json!({ "members": [] });
    let err = parse_group_members(&group).expect_err("empty roster must fail closed");
    assert!(
        matches!(err, WireError::Membership(_)),
        "empty member set must yield WireError::Membership, got {err:?}"
    );
}

#[test]
fn empty_number_rejects_the_whole_parse() {
    // A single empty number anywhere rejects the entire roster (fail-closed).
    let group = json!({
        "members": [
            { "number": "+15551230001" },
            { "number": "" },
            { "number": "+15551230003" }
        ]
    });
    let err = parse_group_members(&group).expect_err("empty number must fail closed");
    assert!(
        matches!(err, WireError::Membership(_)),
        "an empty member number must yield WireError::Membership, got {err:?}"
    );
}

#[test]
fn malformed_number_rejects_the_whole_parse() {
    // Non-conforming (missing '+', non-digit, over-long) numbers reject the parse.
    for bad in [
        "15551230001",       // no leading '+'
        "+1555abc0001",      // non-digit body
        "+1234567890123456", // 16 digits — exceeds the E.164 max of 15
        "+",                 // '+' with no digits
    ] {
        let group = json!({
            "members": [
                { "number": "+15551230001" },
                { "number": bad }
            ]
        });
        let err = parse_group_members(&group)
            .unwrap_err_or_else_msg(&format!("malformed number {bad:?} must be rejected"));
        assert!(
            matches!(err, WireError::Membership(_)),
            "malformed number {bad:?} must yield WireError::Membership, got {err:?}"
        );
    }
}

/// Locks the anti-PII property: the membership error MUST NOT echo any member
/// number in its `Display` or `Debug` output. Regressions here would leak
/// operator phone numbers into logs.
#[test]
fn parse_failure_message_does_not_leak_member_numbers() {
    let secret_ok = "+19998887777";
    let secret_bad = "+1BADNUMBER00"; // malformed sentinel with a distinctive tail
    let group = json!({
        "members": [
            { "number": secret_ok },
            { "number": secret_bad }
        ]
    });
    let err = parse_group_members(&group).expect_err("malformed roster must fail");

    let shown = format!("{err}");
    let debugged = format!("{err:?}");
    for needle in [secret_ok, secret_bad, "9998887777", "BADNUMBER"] {
        assert!(
            !shown.contains(needle),
            "Display of membership error leaked {needle:?}: {shown}"
        );
        assert!(
            !debugged.contains(needle),
            "Debug of membership error leaked {needle:?}: {debugged}"
        );
    }
}

/// Tiny helper so the malformed-number loop reads cleanly.
trait ExpectErrMsg<T> {
    fn unwrap_err_or_else_msg(self, msg: &str) -> WireError
    where
        Self: Sized;
}

impl ExpectErrMsg<Vec<String>> for Result<Vec<String>, WireError> {
    fn unwrap_err_or_else_msg(self, msg: &str) -> WireError {
        match self {
            Ok(v) => panic!("{msg}: expected Err, got Ok({v:?})"),
            Err(e) => e,
        }
    }
}
