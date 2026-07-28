//! Integration tests — the 5-step audit workflow.
//!
//! Ported from upstream `tests/integration/test_audit_workflow.py`.

mod common;

use amplihack_supply_chain_audit::{AuditConfig, Severity, run_audit};
use common::{temp_repo, write_file};
use regex::Regex;

const UNPINNED_WF: &str = "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n\
         - uses: actions/checkout@v4\n";

fn audit(root: &std::path::Path, scope: &str) -> amplihack_supply_chain_audit::AuditResult {
    run_audit(&AuditConfig::new(root).with_scope(scope)).expect("audit ok")
}

// ── Step 1: scope detection ──────────────────────────────────────────────────

#[test]
fn scope_detection_result_in_report_header() {
    let repo = temp_repo();
    write_file(repo.path(), "requirements.txt", "requests==2.31.0\n");
    let report = audit(repo.path(), "all").render_report();
    assert!(report.to_lowercase().contains("python"));
    assert!(report.to_lowercase().contains("skipped"));
}

#[test]
fn gha_not_detected_when_no_workflows() {
    let repo = temp_repo();
    write_file(repo.path(), "requirements.txt", "requests==2.31.0\n");
    let result = audit(repo.path(), "all");
    assert!(!result.active_dimensions().contains(&1));
    assert!(result.skipped_dimensions().contains(&1));
}

#[test]
fn active_and_skipped_partition_1_to_12() {
    let repo = temp_repo();
    let result = audit(repo.path(), "all");
    let mut all: Vec<u32> = result
        .active_dimensions()
        .iter()
        .chain(result.skipped_dimensions())
        .copied()
        .collect();
    all.sort_unstable();
    all.dedup();
    assert_eq!(all, (1..=12).collect::<Vec<_>>());
}

// ── Step 2: static analysis ──────────────────────────────────────────────────

#[test]
fn every_finding_has_file_and_nonnegative_line() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", UNPINNED_WF);
    for f in audit(repo.path(), "gha").findings() {
        assert!(!f.file().is_empty());
        // line() returns u32 — always >= 0.
        let _ = f.line();
    }
}

#[test]
fn file_paths_are_relative_posix() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", UNPINNED_WF);
    for f in audit(repo.path(), "gha").findings() {
        assert!(!f.file().starts_with('/'), "absolute path: {}", f.file());
        assert!(!f.file().contains('\\'), "windows sep: {}", f.file());
    }
}

#[test]
fn every_finding_has_current_and_expected() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", UNPINNED_WF);
    for f in audit(repo.path(), "gha").findings() {
        assert!(!f.current_value().is_empty());
        assert!(!f.expected_value().is_empty());
    }
}

// ── Step 3: severity scoring ─────────────────────────────────────────────────

#[test]
fn all_findings_have_valid_severity() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push, pull_request_target]\njobs:\n  test:\n\
             runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n\
             - run: echo \"${{ secrets.TOKEN }}\"\n",
    );
    let valid = Severity::all();
    for f in audit(repo.path(), "gha").findings() {
        assert!(valid.contains(&f.severity()));
    }
}

#[test]
fn pull_request_target_elevates_to_critical() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push, pull_request_target]\njobs:\n  test:\n\
             runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n",
    );
    assert!(
        audit(repo.path(), "gha")
            .findings()
            .iter()
            .any(|f| f.severity() == Severity::Critical)
    );
}

#[test]
fn min_severity_filter_suppresses_lower_findings() {
    let repo = temp_repo();
    write_file(repo.path(), "requirements.txt", "requests==2.31.0\n");
    write_file(repo.path(), ".github/workflows/ci.yml", UNPINNED_WF);
    let cfg = AuditConfig::new(repo.path())
        .with_scope("all")
        .with_min_severity(Severity::High);
    let result = run_audit(&cfg).unwrap();
    for f in result.findings() {
        assert!(matches!(f.severity(), Severity::Critical | Severity::High));
    }
}

// ── Step 4: report generation ────────────────────────────────────────────────

#[test]
fn report_has_required_header_fields() {
    let repo = temp_repo();
    let report = audit(repo.path(), "all").render_report();
    assert!(report.contains("Supply Chain Audit Report"));
    assert!(report.contains("Date"));
    assert!(report.contains("Scope"));
    assert!(report.contains("Skipped"));
}

#[test]
fn report_summary_table_has_all_severities() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", UNPINNED_WF);
    let report = audit(repo.path(), "gha").render_report();
    for sev in ["Critical", "High", "Medium", "Info"] {
        assert!(report.contains(sev));
    }
}

#[test]
fn findings_ordered_critical_first() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push, pull_request_target]\njobs:\n  test:\n\
             runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n",
    );
    write_file(repo.path(), "requirements.txt", "requests==2.31.0\n");
    let report = audit(repo.path(), "all").render_report();
    if let (Some(c), Some(h)) = (report.find("CRITICAL"), report.find("HIGH")) {
        assert!(c < h);
    }
}

#[test]
fn report_includes_slsa_section() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n\
             - run: echo hello\n",
    );
    assert!(audit(repo.path(), "gha").render_report().contains("SLSA"));
}

#[test]
fn report_includes_next_steps_section() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", UNPINNED_WF);
    let report = audit(repo.path(), "gha").render_report();
    assert!(report.contains("Next Steps") || report.contains("Recommended"));
}

#[test]
fn empty_report_lists_checked_and_skipped() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\npermissions: read-all\njobs:\n  build:\n\
             runs-on: ubuntu-latest\n    steps:\n\
             - uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683  # v4.2.2\n",
    );
    let report = audit(repo.path(), "gha").render_report().to_lowercase();
    assert!(report.contains("checked"));
    assert!(report.contains("skipped"));
}

#[test]
fn finding_ids_follow_severity_nnn_format() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push, pull_request_target]\njobs:\n  test:\n\
             runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n",
    );
    let re = Regex::new(r"^(CRITICAL|HIGH|MEDIUM|INFO)-\d{3}$").unwrap();
    for f in audit(repo.path(), "gha").findings() {
        assert!(re.is_match(f.id()), "bad id: {}", f.id());
    }
}

#[test]
fn finding_ids_unique_within_report() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n\
             - uses: actions/checkout@v4\n      - uses: actions/setup-python@v5\n\
             - uses: actions/setup-node@v4\n",
    );
    let result = audit(repo.path(), "gha");
    let ids: Vec<&str> = result.findings().iter().map(|f| f.id()).collect();
    let mut unique = ids.clone();
    unique.sort_unstable();
    unique.dedup();
    assert_eq!(ids.len(), unique.len(), "duplicate ids: {ids:?}");
}

// ── Step 5: remediation prioritisation ───────────────────────────────────────

#[test]
fn next_steps_delegates_lock_files_to_dependency_resolver() {
    let repo = temp_repo();
    write_file(repo.path(), "requirements.txt", "requests==2.31.0\n");
    assert!(
        audit(repo.path(), "python")
            .render_report()
            .contains("dependency-resolver")
    );
}

#[test]
fn next_steps_recommends_pre_commit_hooks() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", UNPINNED_WF);
    let report = audit(repo.path(), "gha").render_report();
    assert!(report.contains("pre-commit") || report.contains("pre_commit"));
}

// ── Inter-skill handoffs ─────────────────────────────────────────────────────

#[test]
fn dependency_resolver_handoff_template_fields() {
    let repo = temp_repo();
    write_file(repo.path(), "requirements.txt", "requests==2.31.0\n");
    let handoff = audit(repo.path(), "python")
        .get_handoff("dependency-resolver")
        .expect("handoff present");
    assert!(handoff.contains("Ecosystems with lock file issues"));
    assert!(handoff.contains("CI validation commands"));
}

#[test]
fn pre_commit_manager_handoff_template_fields() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", UNPINNED_WF);
    let handoff = audit(repo.path(), "gha")
        .get_handoff("pre-commit-manager")
        .expect("handoff present");
    assert!(handoff.contains("Hooks to install"));
    assert!(handoff.contains("Findings this would have prevented"));
}

#[test]
fn cybersecurity_analyst_handoff_includes_posture_summary() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push, pull_request_target]\njobs:\n  test:\n\
             runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n",
    );
    if let Some(handoff) = audit(repo.path(), "gha").get_handoff("cybersecurity-analyst") {
        assert!(handoff.contains("Critical:"));
        assert!(handoff.contains("High:"));
    }
}

#[test]
fn silent_degradation_handoff_for_continue_on_error() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\npermissions: read-all\njobs:\n  build:\n\
             runs-on: ubuntu-latest\n    steps:\n\
             - uses: some/security-scan@<sha>  # v1\n        continue-on-error: true\n",
    );
    if let Some(handoff) = audit(repo.path(), "gha").get_handoff("silent-degradation-audit") {
        assert!(handoff.contains("continue-on-error"));
        let lower = handoff.to_lowercase();
        assert!(lower.contains("security gates") || lower.contains("enforcing"));
    }
}
