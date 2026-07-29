//! Unit / integration tests — named error conditions.
//!
//! Ported from upstream `tests/unit/test_error_conditions.py`. Covers the
//! `INVALID_SCOPE`, `PATH_TRAVERSAL`, and `ACCEPTED_RISKS_OVERFLOW` conditions
//! plus offline-still-reported behaviour. Tests that relied on monkeypatching
//! Python's `subprocess`/`shutil` are re-expressed as deterministic checks.

mod common;

use amplihack_supply_chain_audit::{AuditConfig, run_audit};
use common::{temp_repo, write_file};

// ── INVALID_SCOPE ────────────────────────────────────────────────────────────

#[test]
fn invalid_scope_error_code() {
    let repo = temp_repo();
    let cfg = AuditConfig::new(repo.path()).with_scope("kubernetes");
    let err = run_audit(&cfg).unwrap_err();
    assert_eq!(err.error_code(), "INVALID_SCOPE");
}

#[test]
fn invalid_scope_message_lists_valid_scopes() {
    let repo = temp_repo();
    let cfg = AuditConfig::new(repo.path()).with_scope("invalid");
    let err = run_audit(&cfg).unwrap_err();
    let msg = err.to_string();
    for valid in [
        "gha",
        "containers",
        "python",
        "node",
        "go",
        "rust",
        "dotnet",
        "all",
    ] {
        assert!(msg.contains(valid), "scope '{valid}' missing from: {msg}");
    }
}

// ── PATH_TRAVERSAL ───────────────────────────────────────────────────────────

#[test]
fn path_traversal_error_code() {
    let repo = temp_repo();
    let bad = repo.path().join("..").join("other");
    let cfg = AuditConfig::new(bad).with_scope("all");
    let err = run_audit(&cfg).unwrap_err();
    assert_eq!(err.error_code(), "PATH_TRAVERSAL");
}

#[test]
fn path_traversal_message_mentions_rejected_path() {
    let repo = temp_repo();
    let bad = repo.path().join("..").join("sensitive");
    let cfg = AuditConfig::new(bad).with_scope("all");
    let err = run_audit(&cfg).unwrap_err();
    let msg = err.to_string();
    assert!(
        msg.contains("..") || msg.contains("PATH_TRAVERSAL"),
        "message: {msg}"
    );
}

// ── ACCEPTED_RISKS_OVERFLOW ──────────────────────────────────────────────────

#[test]
fn overflow_file_aborts_audit() {
    let repo = temp_repo();
    // Well over 64 KiB.
    let content = "- id: HIGH-001\n".repeat(5000);
    write_file(repo.path(), ".supply-chain-accepted-risks.yml", &content);
    let cfg = AuditConfig::new(repo.path()).with_scope("all");
    let err = run_audit(&cfg).unwrap_err();
    assert_eq!(err.error_code(), "ACCEPTED_RISKS_OVERFLOW");
}

#[test]
fn overflow_error_instructs_user_to_split_file() {
    let repo = temp_repo();
    let content = "x".repeat(64 * 1024 + 100);
    write_file(repo.path(), ".supply-chain-accepted-risks.yml", &content);
    let cfg = AuditConfig::new(repo.path()).with_scope("all");
    let err = run_audit(&cfg).unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("split") || msg.contains("archive"),
        "message: {msg}"
    );
}

#[test]
fn exactly_64kb_is_accepted() {
    let repo = temp_repo();
    let content = "x".repeat(64 * 1024);
    write_file(repo.path(), ".supply-chain-accepted-risks.yml", &content);
    let cfg = AuditConfig::new(repo.path()).with_scope("all");
    // Exactly 64 KiB must NOT trigger overflow. Any other outcome (Ok or a
    // different error) is acceptable — but not ACCEPTED_RISKS_OVERFLOW.
    if let Err(err) = run_audit(&cfg) {
        assert_ne!(err.error_code(), "ACCEPTED_RISKS_OVERFLOW");
    }
}

// ── Offline findings still reported without external tools ───────────────────

#[test]
fn offline_detectable_findings_still_reported() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n\
             steps:\n      - uses: actions/checkout@v4\n",
    );
    let cfg = AuditConfig::new(repo.path()).with_scope("gha");
    let result = run_audit(&cfg).expect("audit should succeed offline");
    assert!(!result.findings().is_empty());
    assert!(result.findings().iter().any(|f| f.offline_detectable()));
}
