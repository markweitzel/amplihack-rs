//! Unit tests — report schema compliance.
//!
//! Ported from upstream `tests/unit/test_report_schema.py`.

use amplihack_supply_chain_audit::report::AuditReport;
use amplihack_supply_chain_audit::{Finding, Severity, SlsaAssessment};

fn empty_report(active: Vec<u32>, skipped: Vec<u32>) -> AuditReport {
    AuditReport::builder(Vec::new(), active, skipped).build()
}

fn finding(id: &str, dim: u32, sev: Severity, line: i64) -> Finding {
    Finding::builder(id, dim, sev, "f.yml", line, "x", "y", "r", true)
        .build()
        .expect("valid finding")
}

// ── Report structure: 5 required sections + header fields ────────────────────

#[test]
fn report_has_date_field() {
    let r = empty_report(vec![1, 2, 3, 4], (5..=12).collect());
    assert!(r.render().contains("**Date**:"));
}

#[test]
fn report_has_root_field() {
    let r = AuditReport::builder(Vec::new(), Vec::new(), (1..=12).collect())
        .root("/repo")
        .build();
    assert!(r.render().contains("**Root**:"));
}

#[test]
fn report_has_scope_field() {
    let r = AuditReport::builder(Vec::new(), vec![1], (2..=12).collect())
        .scope(vec!["gha".to_string()])
        .build();
    assert!(r.render().contains("**Scope**:"));
}

#[test]
fn report_has_skipped_field() {
    let r = empty_report(vec![1, 2, 3, 4], vec![5, 6, 7, 8, 9, 10, 11, 12]);
    assert!(r.render().contains("**Skipped**:"));
}

#[test]
fn report_has_tool_availability_field() {
    let r = empty_report(Vec::new(), (1..=12).collect());
    assert!(r.render().contains("**Tool availability**:"));
}

#[test]
fn report_has_summary_section() {
    let r = empty_report(Vec::new(), (1..=12).collect());
    assert!(r.render().contains("### Summary"));
}

#[test]
fn report_has_findings_section() {
    let r = empty_report(Vec::new(), (1..=12).collect());
    assert!(r.render().contains("### Findings"));
}

#[test]
fn report_has_slsa_readiness_section() {
    let r = empty_report(vec![1, 2, 3, 4], (5..=12).collect());
    assert!(r.render().contains("### SLSA Readiness"));
}

#[test]
fn report_has_next_steps_section() {
    let r = empty_report(Vec::new(), (1..=12).collect());
    assert!(r.render().contains("### Recommended Next Steps"));
}

// ── Summary table ────────────────────────────────────────────────────────────

#[test]
fn summary_table_has_all_severity_rows() {
    let r = empty_report(Vec::new(), (1..=12).collect());
    let rendered = r.render();
    for sev in ["Critical", "High", "Medium", "Info"] {
        assert!(rendered.contains(sev), "missing severity row {sev}");
    }
}

#[test]
fn summary_table_counts_are_accurate() {
    let findings = vec![
        finding("CRITICAL-001", 1, Severity::Critical, 1),
        finding("HIGH-001", 2, Severity::High, 2),
        finding("HIGH-002", 2, Severity::High, 3),
    ];
    let r = AuditReport::builder(findings, vec![1, 2], (3..=12).collect()).build();
    let rendered = r.render();
    assert!(rendered.contains('1'));
    assert!(rendered.contains('2'));
}

#[test]
fn summary_total_row_present() {
    let findings = vec![finding("HIGH-001", 1, Severity::High, 1)];
    let r = AuditReport::builder(findings, vec![1], (2..=12).collect()).build();
    assert!(r.render().contains("**Total**"));
}

// ── Finding block format ─────────────────────────────────────────────────────

#[test]
fn finding_block_has_id_and_dimension() {
    let f = Finding::builder(
        "HIGH-001",
        1,
        Severity::High,
        ".github/workflows/ci.yml",
        8,
        "uses: actions/checkout@v4",
        "uses: actions/checkout@<sha>  # v4",
        "Mutable ref.",
        true,
    )
    .build()
    .unwrap();
    let r = AuditReport::builder(vec![f], vec![1], (2..=12).collect()).build();
    let rendered = r.render();
    assert!(rendered.contains("HIGH-001"));
    assert!(rendered.contains("Dim 1"));
}

#[test]
fn finding_block_has_severity_line() {
    let f = finding("HIGH-001", 1, Severity::High, 1);
    let r = AuditReport::builder(vec![f], vec![1], (2..=12).collect()).build();
    assert!(r.render().contains("**Severity**:"));
}

#[test]
fn finding_block_has_file_colon_line_reference() {
    let f = Finding::builder(
        "HIGH-001",
        1,
        Severity::High,
        ".github/workflows/ci.yml",
        8,
        "x",
        "y",
        "r",
        true,
    )
    .build()
    .unwrap();
    let r = AuditReport::builder(vec![f], vec![1], (2..=12).collect()).build();
    assert!(r.render().contains(".github/workflows/ci.yml:8"));
}

#[test]
fn finding_block_has_why_rationale() {
    let f = Finding::builder(
        "HIGH-001",
        1,
        Severity::High,
        "f.yml",
        1,
        "x",
        "y",
        "Mutable semver tag allows silent replacement.",
        true,
    )
    .build()
    .unwrap();
    let r = AuditReport::builder(vec![f], vec![1], (2..=12).collect()).build();
    let rendered = r.render();
    assert!(rendered.contains("**Why**:"));
    assert!(rendered.contains("Mutable semver tag"));
}

// ── SLSA assessment ──────────────────────────────────────────────────────────

#[test]
fn slsa_table_has_required_rows() {
    let slsa = SlsaAssessment::new(true, true, false, false);
    let rendered = slsa.render();
    assert!(rendered.contains("Build is scripted"));
    assert!(rendered.contains("Build runs on hosted CI"));
    assert!(rendered.contains("Provenance generated"));
    assert!(rendered.contains("Action refs SHA-pinned"));
}

#[test]
fn slsa_table_shows_current_level_l1() {
    let slsa = SlsaAssessment::new(true, true, false, false);
    assert_eq!(slsa.current_level(), "L1");
    assert!(slsa.render().contains("L1"));
}

#[test]
fn slsa_l1_blockers_to_l2_listed() {
    let slsa = SlsaAssessment::new(true, true, false, false);
    let rendered = slsa.render().to_lowercase();
    assert!(rendered.contains("l2"));
    assert!(rendered.contains("provenance") || rendered.contains("blocker"));
}

#[test]
fn slsa_l2_if_provenance_generated_and_pinned() {
    let slsa = SlsaAssessment::new(true, true, true, true);
    assert_eq!(slsa.current_level(), "L2");
    assert!(slsa.render().contains("L2"));
}

#[test]
fn slsa_l0_when_build_not_scripted() {
    let slsa = SlsaAssessment::new(false, false, false, false);
    assert_eq!(slsa.current_level(), "L0");
}
