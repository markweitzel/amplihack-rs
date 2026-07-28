//! Unit tests — external tool integration layer.
//!
//! Ported from upstream `tests/unit/test_external_tools.py`. Subprocess-mocking
//! tests are re-expressed as deterministic checks over the timeout table,
//! circuit breaker, availability map, and install metadata.

use amplihack_supply_chain_audit::external_tools::{
    CircuitBreaker, TOOL_NAMES, ToolClient, check_missing_tools, check_tool_availability,
    tool_timeout,
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

#[test]
fn breaker_starts_closed() {
    let cb = CircuitBreaker::new();
    assert!(!cb.is_open());
}

#[test]
fn breaker_opens_after_failure_threshold() {
    let mut cb = CircuitBreaker::with_config(3, 60);
    for _ in 0..3 {
        cb.record_failure();
    }
    assert!(cb.is_open());
}

#[test]
fn breaker_does_not_open_before_threshold() {
    let mut cb = CircuitBreaker::with_config(3, 60);
    cb.record_failure();
    cb.record_failure();
    assert!(!cb.is_open());
}

#[test]
fn breaker_resets_on_success() {
    let mut cb = CircuitBreaker::with_config(2, 60);
    cb.record_failure();
    cb.record_failure();
    assert!(cb.is_open());
    cb.record_success();
    assert!(!cb.is_open());
}

#[test]
fn breaker_half_open_probe_after_reset_timeout() {
    // reset_timeout of 0 means the reset window has always already elapsed,
    // so a half-open probe is permitted immediately after opening.
    let mut cb = CircuitBreaker::with_config(1, 0);
    cb.record_failure();
    assert!(!cb.is_open());
}

#[test]
fn breaker_manual_reset() {
    let mut cb = CircuitBreaker::with_config(1, 60);
    cb.record_failure();
    assert!(cb.is_open());
    cb.reset();
    assert!(!cb.is_open());
}

// ── Availability + client ────────────────────────────────────────────────────

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
fn tool_client_reports_name_and_timeout() {
    let client = ToolClient::new("gh").expect("gh is a known tool");
    assert_eq!(client.name(), "gh");
    assert_eq!(client.timeout(), 15);
}

#[test]
fn unknown_tool_client_is_none() {
    assert!(ToolClient::new("docker").is_none());
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
