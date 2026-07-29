//! Unit tests — external tool availability layer.
//!
//! Ported from upstream `tests/unit/test_external_tools.py`. Subprocess-mocking
//! tests are re-expressed as deterministic checks over the timeout table,
//! availability map, and install metadata.

use amplihack_supply_chain_audit::external_tools::{
    TOOL_NAMES, check_missing_tools, check_tool_availability, tool_timeout,
};

// ── Timeout constants ────────────────────────────────────────────────────────

#[test]
fn gh_timeout_is_15s() {
    assert_eq!(tool_timeout("gh"), Some(15));
}

#[test]
fn crane_timeout_is_20s() {
    assert_eq!(tool_timeout("crane"), Some(20));
}

#[test]
fn syft_timeout_is_120s() {
    assert_eq!(tool_timeout("syft"), Some(120));
}

#[test]
fn grype_timeout_is_60s() {
    assert_eq!(tool_timeout("grype"), Some(60));
}

#[test]
fn cosign_timeout_is_30s() {
    assert_eq!(tool_timeout("cosign"), Some(30));
}

#[test]
fn all_five_tools_have_timeouts() {
    for tool in ["gh", "crane", "syft", "grype", "cosign"] {
        assert!(tool_timeout(tool).is_some(), "missing timeout for {tool}");
    }
    assert_eq!(TOOL_NAMES.len(), 5);
}

#[test]
fn unknown_tool_has_no_timeout() {
    assert_eq!(tool_timeout("docker"), None);
}

// ── Circuit breaker ──────────────────────────────────────────────────────────

// (Removed: the in-process circuit breaker and per-tool client were inert
// scaffolding — no code path executed external commands — and were deleted for
// Zero-BS compliance. Availability is reported directly by the functions below.)

// ── Availability + install metadata ──────────────────────────────────────────

#[test]
fn availability_map_covers_all_tools() {
    let status = check_tool_availability();
    for tool in TOOL_NAMES {
        let s = status.get(tool).expect("tool present in status map");
        assert!(
            s.contains("available") || s.contains("unavailable"),
            "unexpected status for {tool}: {s}"
        );
    }
}

#[test]
fn missing_tools_have_install_metadata() {
    // Whatever tools are missing on this host, each entry must carry install
    // metadata (name + at least one install option). If all are present, the
    // list is simply empty — both outcomes are valid.
    for missing in check_missing_tools() {
        assert!(TOOL_NAMES.contains(&missing.name.as_str()));
        assert!(
            !missing.install_options.is_empty(),
            "no options for {}",
            missing.name
        );
    }
}
