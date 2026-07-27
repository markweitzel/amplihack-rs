//! FIX 3 (RED) — per-chunk group-membership re-verification (fail-closed).
//!
//! Contract under test (currently UNIMPLEMENTED — expected to FAIL until FIX 3
//! lands). Before EACH outbound `send_group` chunk of a multi-part relay, the
//! sender re-reads the live group roster (`group_members()`) and re-checks it
//! against the roster verified at the start of the post. The decision kernel:
//!
//!   `amplihack_signal::transport::verify_membership(current, expected)
//!        -> MembershipVerdict`
//!
//! returns `Verified` only when the current roster is the SAME SET as the
//! expected one (order- and duplicate-insensitive). Any drift — a member
//! removed, a member altered, or a foreign member injected mid-body — yields
//! `Withhold`, at which point the caller stops the remaining chunks and surfaces
//! the withheld relay via the existing WITHHOLDING log path (no silent drop).
//!
//! This is the pure, deterministic core of FIX 3. The per-chunk wiring in the
//! relay loop calls `verify_membership` before every chunk; these tests lock the
//! fail-closed decision it depends on, plus the `parse_group_members` -> verify
//! pipeline that a real mid-body re-read exercises.
#![cfg(feature = "signal")]

use amplihack_signal::transport::{MembershipVerdict, parse_group_members, verify_membership};
use serde_json::json;

fn roster(nums: &[&str]) -> Vec<String> {
    nums.iter().map(|s| (*s).to_string()).collect()
}

#[test]
fn identical_roster_is_verified() {
    let expected = roster(&["+15551230001", "+15551230002"]);
    let current = roster(&["+15551230001", "+15551230002"]);
    assert_eq!(
        verify_membership(&current, &expected),
        MembershipVerdict::Verified
    );
}

#[test]
fn reordered_or_duplicated_same_set_is_verified() {
    // Set equality, not sequence equality: a benign reorder (and a duplicate
    // entry) that leaves the member SET unchanged must not trip a false
    // withhold.
    let expected = roster(&["+15551230001", "+15551230002", "+15551230003"]);
    let current = roster(&[
        "+15551230003",
        "+15551230001",
        "+15551230002",
        "+15551230001",
    ]);
    assert_eq!(
        verify_membership(&current, &expected),
        MembershipVerdict::Verified
    );
}

#[test]
fn removed_member_withholds() {
    let expected = roster(&["+15551230001", "+15551230002", "+15551230003"]);
    let current = roster(&["+15551230001", "+15551230003"]); // 002 dropped mid-body
    assert_eq!(
        verify_membership(&current, &expected),
        MembershipVerdict::Withhold,
        "a member removed mid-body must fail closed and stop remaining chunks"
    );
}

#[test]
fn altered_member_withholds() {
    let expected = roster(&["+15551230001", "+15551230002"]);
    let current = roster(&["+15551230001", "+15559999999"]); // 002 -> attacker number
    assert_eq!(
        verify_membership(&current, &expected),
        MembershipVerdict::Withhold,
        "a member number altered mid-body must fail closed"
    );
}

#[test]
fn injected_foreign_member_withholds() {
    let expected = roster(&["+15551230001", "+15551230002"]);
    let current = roster(&["+15551230001", "+15551230002", "+15550000042"]); // injected
    assert_eq!(
        verify_membership(&current, &expected),
        MembershipVerdict::Withhold,
        "a foreign member injected mid-body must fail closed"
    );
}

/// End-to-end decision pipeline: the roster snapshotted at post start, then a
/// tampered roster re-read before the next chunk, must classify as `Withhold`
/// — this is the exact point where FIX 3 halts the remaining chunks and logs
/// the withhold.
#[test]
fn parsed_midbody_reread_detects_tamper_and_withholds() {
    let baseline_result = json!({
        "members": [
            { "number": "+15551230001" },
            { "number": "+15551230002" }
        ]
    });
    let expected = parse_group_members(&baseline_result).expect("baseline roster parses");

    // Between chunks the roster is re-read and a foreign member has appeared.
    let midbody_result = json!({
        "members": [
            { "number": "+15551230001" },
            { "number": "+15551230002" },
            { "number": "+15550000042" }
        ]
    });
    let current = parse_group_members(&midbody_result).expect("mid-body roster parses");

    assert_eq!(
        verify_membership(&current, &expected),
        MembershipVerdict::Withhold,
        "mid-body membership drift must withhold the remaining relay chunks"
    );
}
