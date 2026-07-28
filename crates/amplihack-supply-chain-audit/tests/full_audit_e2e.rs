//! End-to-end tests — full audit execution pipeline.
//!
//! Ported from upstream `tests/e2e/test_full_audit.py`.

mod common;

use amplihack_supply_chain_audit::{AuditConfig, AuditResult, Severity, run_audit};
use common::{temp_repo, write_file};
use regex::Regex;

const UNPINNED_WF: &str = "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n    steps:\n\
         - uses: actions/checkout@v4\n";

fn audit(root: &std::path::Path, scope: &str) -> AuditResult {
    run_audit(&AuditConfig::new(root).with_scope(scope)).expect("audit ok")
}

/// Clean GHA-only repo (pinned SHA + permissions) used by the empty-report tests.
fn gha_only_repo() -> tempfile::TempDir {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\npermissions: read-all\njobs:\n  build:\n\
             runs-on: ubuntu-latest\n    steps:\n\
             - uses: actions/checkout@11bd71901bbe5b1630ceea73d27597364c9af683  # v4.2.2\n",
    );
    repo
}

// ── min-severity filtering ───────────────────────────────────────────────────

#[test]
fn min_severity_high_excludes_medium_info() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", UNPINNED_WF);
    let all = run_audit(
        &AuditConfig::new(repo.path())
            .with_scope("gha")
            .with_min_severity(Severity::Info),
    )
    .unwrap();
    let high = run_audit(
        &AuditConfig::new(repo.path())
            .with_scope("gha")
            .with_min_severity(Severity::High),
    )
    .unwrap();
    assert!(high.findings().len() <= all.findings().len());
    for f in high.findings() {
        assert!(matches!(f.severity(), Severity::Critical | Severity::High));
    }
}

#[test]
fn min_severity_critical_shows_only_critical() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push, pull_request_target]\njobs:\n  test:\n\
             runs-on: ubuntu-latest\n    steps:\n      - uses: actions/checkout@v4\n",
    );
    let result = run_audit(
        &AuditConfig::new(repo.path())
            .with_scope("gha")
            .with_min_severity(Severity::Critical),
    )
    .unwrap();
    for f in result.findings() {
        assert_eq!(f.severity(), Severity::Critical);
    }
}

#[test]
fn min_severity_in_report_header() {
    let repo = temp_repo();
    let result = run_audit(
        &AuditConfig::new(repo.path())
            .with_scope("all")
            .with_min_severity(Severity::High),
    )
    .unwrap();
    assert!(result.render_report().contains("High"));
}

// ── scope filtering ──────────────────────────────────────────────────────────

#[test]
fn scope_gha_only_checks_dims_1_to_4() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", UNPINNED_WF);
    write_file(repo.path(), "requirements.txt", "requests==2.31.0\n");
    for f in audit(repo.path(), "gha").findings() {
        assert!(
            (1..=4).contains(&f.dimension()),
            "dim {} outside gha",
            f.dimension()
        );
    }
}

#[test]
fn scope_python_only_checks_dim_8() {
    let repo = temp_repo();
    write_file(repo.path(), "requirements.txt", "requests==2.31.0\n");
    write_file(repo.path(), ".github/workflows/ci.yml", UNPINNED_WF);
    for f in audit(repo.path(), "python").findings() {
        assert_eq!(f.dimension(), 8, "dim outside python scope");
    }
}

#[test]
fn scope_comma_separated_multi_scope() {
    let repo = temp_repo();
    write_file(repo.path(), "requirements.txt", "requests==2.31.0\n");
    write_file(repo.path(), "package.json", "{\"name\": \"app\"}\n");
    for f in audit(repo.path(), "python,node").findings() {
        assert!(
            matches!(f.dimension(), 8 | 10),
            "dim {} outside python,node",
            f.dimension()
        );
    }
}

// ── report format compliance ─────────────────────────────────────────────────

#[test]
fn report_has_supply_chain_audit_report_heading() {
    let repo = temp_repo();
    assert!(
        audit(repo.path(), "all")
            .render_report()
            .contains("## Supply Chain Audit Report")
    );
}

#[test]
fn report_date_format_is_iso_8601() {
    let repo = temp_repo();
    let report = audit(repo.path(), "all").render_report();
    let re = Regex::new(r"\*\*Date\*\*:\s*(\d{4}-\d{2}-\d{2})").unwrap();
    assert!(re.is_match(&report), "date not in YYYY-MM-DD format");
}

#[test]
fn report_tool_availability_field_present() {
    let repo = temp_repo();
    assert!(
        audit(repo.path(), "all")
            .render_report()
            .to_lowercase()
            .contains("tool availability")
    );
}

#[test]
fn finding_format_contains_all_required_fields() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", UNPINNED_WF);
    let result = audit(repo.path(), "gha");
    let report = result.render_report();
    if !result.findings().is_empty() {
        for field in [
            "**Severity**",
            "**File**",
            "**Current**",
            "**Expected**",
            "**Why**",
        ] {
            assert!(report.contains(field), "missing {field}");
        }
    }
}

#[test]
fn file_line_reference_format() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", UNPINNED_WF);
    let report = audit(repo.path(), "gha").render_report();
    let re = Regex::new(r"`.+:\d+`").unwrap();
    assert!(re.is_match(&report), "no file:line reference found");
}

// ── polyglot repo ────────────────────────────────────────────────────────────

#[test]
fn all_12_dimensions_run_in_polyglot_repo() {
    let repo = temp_repo();
    write_file(
        repo.path(),
        ".github/workflows/ci.yml",
        "name: CI\non: [push]\njobs:\n  build:\n    runs-on: ubuntu-latest\n\
             env:\n      TOKEN: ${{ secrets.MY_TOKEN }}\n    steps:\n\
             - uses: actions/checkout@v4\n",
    );
    write_file(repo.path(), "Dockerfile", "FROM ubuntu:22.04\n");
    write_file(repo.path(), "requirements.txt", "requests==2.31.0\n");
    write_file(repo.path(), "package.json", "{\"name\": \"app\"}\n");
    write_file(
        repo.path(),
        "go.mod",
        "module github.com/org/app\n\ngo 1.22\n",
    );
    write_file(
        repo.path(),
        "Cargo.toml",
        "[package]\nname = 'tool'\nversion = '0.1.0'\n",
    );
    write_file(
        repo.path(),
        "App.csproj",
        "<Project Sdk=\"Microsoft.NET.Sdk\" />\n",
    );

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

#[test]
fn polyglot_repo_produces_findings_across_multiple_dimensions() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", UNPINNED_WF);
    write_file(repo.path(), "Dockerfile", "FROM ubuntu:latest\n");
    write_file(repo.path(), "requirements.txt", "requests==2.31.0\n");

    let result = audit(repo.path(), "all");
    let mut dims: Vec<u32> = result.findings().iter().map(|f| f.dimension()).collect();
    dims.sort_unstable();
    dims.dedup();
    assert!(
        dims.len() >= 3,
        "expected findings in >=3 dimensions, got {dims:?}"
    );
}

// ── clean repo → empty report ────────────────────────────────────────────────

#[test]
fn clean_gha_workflow_no_high_or_critical() {
    let repo = gha_only_repo();
    let result = audit(repo.path(), "gha");
    assert!(
        !result
            .findings()
            .iter()
            .any(|f| matches!(f.severity(), Severity::Critical | Severity::High))
    );
}

#[test]
fn empty_report_shows_dimensions_checked() {
    let repo = gha_only_repo();
    let report = audit(repo.path(), "gha").render_report();
    assert!(report.contains("Checked") || report.contains('✅'));
}

#[test]
fn empty_report_shows_dimensions_skipped() {
    let repo = gha_only_repo();
    let report = audit(repo.path(), "gha").render_report();
    assert!(report.contains("Skipped") || report.contains('⏭'));
}

#[test]
fn empty_report_posture_passing() {
    let repo = gha_only_repo();
    let report = audit(repo.path(), "gha").render_report();
    assert!(
        report.to_lowercase().contains("passing") || report.contains('✅'),
        "expected passing posture"
    );
}

// ── accepted-risks suppression flow ──────────────────────────────────────────

const ACCEPTED_RISK_YML: &str = "- id: 'HIGH-001'\n  dimension: 1\n\
     file: '.github/workflows/ci.yml'\n  line: 7\n  rationale: 'Accepted'\n\
     accepted_by: 'security-team'\n  review_date: '2099-12-31'\n";

#[test]
fn accepted_non_critical_finding_appears_as_info() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", UNPINNED_WF);
    write_file(
        repo.path(),
        ".supply-chain-accepted-risks.yml",
        ACCEPTED_RISK_YML,
    );
    let result = audit(repo.path(), "gha");
    for f in result.findings().iter().filter(|f| f.accepted_risk()) {
        assert_eq!(f.severity(), Severity::Info);
    }
}

#[test]
fn accepted_finding_still_visible_in_report() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", UNPINNED_WF);
    write_file(
        repo.path(),
        ".supply-chain-accepted-risks.yml",
        ACCEPTED_RISK_YML,
    );
    let report = audit(repo.path(), "gha").render_report();
    assert!(report.contains("ACCEPTED RISK") || report.to_lowercase().contains("accepted"));
}

#[test]
fn accepted_risks_section_shows_review_date() {
    let repo = temp_repo();
    write_file(repo.path(), ".github/workflows/ci.yml", UNPINNED_WF);
    write_file(
        repo.path(),
        ".supply-chain-accepted-risks.yml",
        ACCEPTED_RISK_YML,
    );
    let report = audit(repo.path(), "gha").render_report();
    assert!(report.contains("2099-12-31"));
}

// ── SBOM advisory ────────────────────────────────────────────────────────────

#[test]
fn sbom_write_advisory_shown_when_requested() {
    let repo = temp_repo();
    write_file(repo.path(), "requirements.txt", "requests==2.31.0\n");
    let result = run_audit(
        &AuditConfig::new(repo.path())
            .with_scope("python")
            .with_generate_sbom(true),
    )
    .unwrap();
    assert!(
        result
            .get_advisory_messages()
            .iter()
            .any(|m| m.contains("SBOM") && m.to_lowercase().contains("advisory"))
    );
}

#[test]
fn sbom_advisory_mentions_gitignore_recommendation() {
    let repo = temp_repo();
    write_file(repo.path(), "requirements.txt", "requests==2.31.0\n");
    let result = run_audit(
        &AuditConfig::new(repo.path())
            .with_scope("python")
            .with_generate_sbom(true),
    )
    .unwrap();
    let joined = result.get_advisory_messages().join(" ").to_lowercase();
    assert!(joined.contains("gitignore"));
}
