//! Unit tests — Finding schema validation.
//!
//! Ported from upstream `tests/unit/test_finding_schema.py`. These define the
//! finding contract (IDs, required fields, constraints, secret redaction) and
//! FAIL until `schema` is implemented.

use amplihack_supply_chain_audit::Severity;
use amplihack_supply_chain_audit::schema::{Finding, FindingId, validate_finding};

/// A valid baseline finding used across constraint tests.
fn valid_high() -> Finding {
    Finding::builder(
        "HIGH-001",
        1,
        Severity::High,
        ".github/workflows/ci.yml",
        8,
        "uses: actions/checkout@v4",
        "uses: actions/checkout@<sha>  # v4",
        "Mutable semver tag allows silent code replacement.",
        true,
    )
    .build()
    .expect("baseline finding should be valid")
}

// ── FindingId format ────────────────────────────────────────────────────────

#[test]
fn valid_critical_id() {
    let fid = FindingId::parse("CRITICAL-001").expect("valid id");
    assert_eq!(fid.severity(), Severity::Critical);
    assert_eq!(fid.sequence(), 1);
}

#[test]
fn valid_high_id() {
    let fid = FindingId::parse("HIGH-042").expect("valid id");
    assert_eq!(fid.severity(), Severity::High);
    assert_eq!(fid.sequence(), 42);
}

#[test]
fn valid_medium_id() {
    let fid = FindingId::parse("MEDIUM-007").expect("valid id");
    assert_eq!(fid.severity(), Severity::Medium);
    assert_eq!(fid.sequence(), 7);
}

#[test]
fn valid_info_id() {
    let fid = FindingId::parse("INFO-001").expect("valid id");
    assert_eq!(fid.severity(), Severity::Info);
    assert_eq!(fid.sequence(), 1);
}

#[test]
fn id_rejects_lowercase_severity() {
    let err = FindingId::parse("critical-001").unwrap_err();
    assert!(
        err.to_string().contains("severity prefix"),
        "message: {err}"
    );
}

#[test]
fn id_rejects_missing_sequence() {
    assert!(FindingId::parse("HIGH-").is_err());
}

#[test]
fn id_rejects_two_digit_sequence() {
    let err = FindingId::parse("HIGH-01").unwrap_err();
    assert!(err.to_string().contains("3-digit"), "message: {err}");
}

#[test]
fn id_rejects_wildcard() {
    let err = FindingId::parse("HIGH-*").unwrap_err();
    assert!(err.to_string().contains("wildcard"), "message: {err}");
}

#[test]
fn id_rejects_invalid_severity_prefix() {
    let err = FindingId::parse("WARNING-001").unwrap_err();
    assert!(err.to_string().contains("severity"), "message: {err}");
}

#[test]
fn duplicate_ids_rejected_by_validate_finding() {
    let a = Finding::builder(
        "HIGH-001",
        1,
        Severity::High,
        ".github/workflows/ci.yml",
        8,
        "actions/checkout@v4",
        "actions/checkout@<sha>  # v4",
        "Mutable ref.",
        true,
    )
    .build()
    .unwrap();
    let b = Finding::builder(
        "HIGH-001",
        1,
        Severity::High,
        ".github/workflows/ci.yml",
        9,
        "actions/setup-python@v5",
        "actions/setup-python@<sha>  # v5",
        "Mutable ref.",
        true,
    )
    .build()
    .unwrap();

    let err = validate_finding(&[a, b]).unwrap_err();
    let msg = err.to_string().to_lowercase();
    assert!(
        msg.contains("duplicate") && msg.contains("id"),
        "message: {err}"
    );
}

// ── Required fields / minimal valid finding ─────────────────────────────────

#[test]
fn minimal_valid_finding() {
    let f = valid_high();
    assert_eq!(f.id(), "HIGH-001");
    assert_eq!(f.dimension(), 1);
}

// ── Field constraints ───────────────────────────────────────────────────────

#[test]
fn dimension_zero_rejected() {
    let err = Finding::builder(
        "HIGH-001",
        0,
        Severity::High,
        "f.yml",
        1,
        "x",
        "y",
        "r",
        true,
    )
    .build()
    .unwrap_err();
    assert!(err.to_string().contains("dimension"), "message: {err}");
}

#[test]
fn dimension_thirteen_rejected() {
    let err = Finding::builder(
        "HIGH-001",
        13,
        Severity::High,
        "f.yml",
        1,
        "x",
        "y",
        "r",
        true,
    )
    .build()
    .unwrap_err();
    assert!(err.to_string().contains("dimension"), "message: {err}");
}

#[test]
fn id_severity_prefix_must_match_finding_severity() {
    // ID says WARNING (invalid prefix) — must be rejected.
    let err = Finding::builder(
        "WARNING-001",
        1,
        Severity::High,
        "f.yml",
        1,
        "x",
        "y",
        "r",
        true,
    )
    .build()
    .unwrap_err();
    assert!(err.to_string().contains("severity"), "message: {err}");
}

#[test]
fn file_must_be_relative_posix_path() {
    let err = Finding::builder(
        "HIGH-001",
        1,
        Severity::High,
        "/home/user/.github/workflows/ci.yml",
        8,
        "x",
        "y",
        "r",
        true,
    )
    .build()
    .unwrap_err();
    assert!(err.to_string().contains("relative"), "message: {err}");
}

#[test]
fn file_rejects_path_traversal() {
    let err = Finding::builder(
        "HIGH-001",
        1,
        Severity::High,
        "../../../etc/passwd",
        1,
        "x",
        "y",
        "r",
        true,
    )
    .build()
    .unwrap_err();
    assert!(err.to_string().contains("traversal"), "message: {err}");
}

#[test]
fn line_zero_valid_for_file_level_findings() {
    let f = Finding::builder(
        "HIGH-001",
        10,
        Severity::High,
        "package.json",
        0,
        "no package-lock.json",
        "add package-lock.json",
        "Lock file absent.",
        true,
    )
    .build()
    .expect("line 0 is valid");
    assert_eq!(f.line(), 0);
}

#[test]
fn line_negative_rejected() {
    let err = Finding::builder(
        "HIGH-001",
        1,
        Severity::High,
        "f.yml",
        -1,
        "x",
        "y",
        "r",
        true,
    )
    .build()
    .unwrap_err();
    assert!(err.to_string().contains("line"), "message: {err}");
}

#[test]
fn tool_required_none_is_valid() {
    let f = Finding::builder(
        "HIGH-001",
        5,
        Severity::High,
        "Dockerfile",
        1,
        "FROM alpine:latest",
        "FROM alpine@sha256:<digest>",
        "Mutable tag.",
        false,
    )
    .build()
    .expect("no tool_required is valid");
    assert_eq!(f.tool_required(), None);
}

#[test]
fn tool_required_crane_is_valid() {
    let f = Finding::builder(
        "HIGH-001",
        5,
        Severity::High,
        "Dockerfile",
        1,
        "FROM alpine:latest",
        "FROM alpine@sha256:<digest>",
        "Mutable tag.",
        false,
    )
    .tool_required("crane")
    .build()
    .expect("crane is an approved tool");
    assert_eq!(f.tool_required(), Some("crane"));
}

#[test]
fn tool_required_invalid_name_rejected() {
    let err = Finding::builder(
        "HIGH-001",
        5,
        Severity::High,
        "Dockerfile",
        1,
        "x",
        "y",
        "r",
        false,
    )
    .tool_required("docker")
    .build()
    .unwrap_err();
    assert!(err.to_string().contains("tool_required"), "message: {err}");
}

// ── Secret redaction ────────────────────────────────────────────────────────

#[test]
fn current_value_with_secret_is_redacted() {
    let f = Finding::builder(
        "CRITICAL-001",
        3,
        Severity::Critical,
        ".github/workflows/ci.yml",
        12,
        "echo 'mysecretvalue123'",
        "Remove secret echo",
        "Secret echoed to log.",
        true,
    )
    .contains_secret(true)
    .build()
    .unwrap();
    let rendered = f.render();
    assert!(!rendered.contains("mysecretvalue123"));
    assert!(rendered.contains("<REDACTED>"));
}

#[test]
fn expected_value_with_secret_is_redacted() {
    let f = Finding::builder(
        "CRITICAL-001",
        6,
        Severity::Critical,
        ".github/workflows/ci.yml",
        5,
        "aws-secret-access-key: ${{ secrets.AWS_SECRET }}",
        "Use OIDC: id-token: write",
        "Static credential.",
        true,
    )
    .contains_secret(true)
    .build()
    .unwrap();
    assert!(f.render().contains("<REDACTED>"));
}
