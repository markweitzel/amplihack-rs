//! Integration tests — the three graded evaluation scenarios.
//!
//! Ported from upstream `tests/integration/test_eval_scenarios.py`.
//!
//! Scenario A: GitHub Actions monorepo (GHA + Python + Node) — 7 planted findings.
//! Scenario B: Containerised Go service (Containers + Go + Credentials) — 5 findings.
//! Scenario C: .NET + Rust mixed repo (.NET + Rust + SLSA) — 6 findings.

mod common;

use amplihack_supply_chain_audit::{AuditConfig, AuditResult, Severity, run_audit};
use common::{copy_fixture_as_repo, temp_repo};
use std::path::Path;

fn audit_scenario(scenario: &str) -> (tempfile::TempDir, AuditResult) {
    let tmp = temp_repo();
    let repo = copy_fixture_as_repo(scenario, tmp.path());
    let result = run_audit(&AuditConfig::new(repo).with_scope("all")).expect("audit ok");
    (tmp, result)
}

fn count(result: &AuditResult, sev: Severity) -> usize {
    result
        .findings()
        .iter()
        .filter(|f| f.severity() == sev)
        .count()
}

// ── Scenario A ───────────────────────────────────────────────────────────────

#[test]
fn scenario_a_f1_unpinned_action_critical() {
    let (_t, r) = audit_scenario("scenario_a");
    assert!(r.findings().iter().any(|f| {
        f.severity() == Severity::Critical
            && f.dimension() == 1
            && f.current_value().contains("checkout@v4")
    }));
}

#[test]
fn scenario_a_f2_pull_request_target_no_permissions() {
    let (_t, r) = audit_scenario("scenario_a");
    assert!(r.findings().iter().any(|f| {
        f.severity() == Severity::Critical
            && f.dimension() == 2
            && f.current_value().contains("pull_request_target")
    }));
}

#[test]
fn scenario_a_f3_secret_echoed_to_log() {
    let (_t, r) = audit_scenario("scenario_a");
    assert!(r.findings().iter().any(|f| {
        f.severity() == Severity::Critical
            && f.dimension() == 3
            && f.rationale().to_lowercase().contains("secret")
    }));
}

#[test]
fn scenario_a_f4_no_hash_pinning_requirements() {
    let (_t, r) = audit_scenario("scenario_a");
    assert!(r.findings().iter().any(|f| {
        f.severity() == Severity::High
            && f.dimension() == 8
            && f.file().contains("requirements.txt")
    }));
}

#[test]
fn scenario_a_f5_no_lock_file() {
    let (_t, r) = audit_scenario("scenario_a");
    assert!(
        r.findings()
            .iter()
            .any(|f| { f.severity() == Severity::High && f.dimension() == 10 && f.line() == 0 })
    );
}

#[test]
fn scenario_a_f6_unversioned_npx() {
    let (_t, r) = audit_scenario("scenario_a");
    assert!(r.findings().iter().any(|f| {
        f.severity() == Severity::High
            && f.dimension() == 10
            && f.current_value().to_lowercase().contains("npx")
    }));
}

#[test]
fn scenario_a_f7_pip_install_without_require_hashes() {
    let (_t, r) = audit_scenario("scenario_a");
    assert!(r.findings().iter().any(|f| {
        f.severity() == Severity::Medium
            && f.dimension() == 8
            && f.expected_value().contains("require-hashes")
    }));
}

#[test]
fn scenario_a_severity_distribution() {
    let (_t, r) = audit_scenario("scenario_a");
    assert!(count(&r, Severity::Critical) >= 2);
    assert!(count(&r, Severity::High) >= 2);
}

#[test]
fn scenario_a_total_findings_at_least_5() {
    let (_t, r) = audit_scenario("scenario_a");
    assert!(r.findings().len() >= 5);
}

#[test]
fn scenario_a_all_findings_offline_detectable() {
    let (_t, r) = audit_scenario("scenario_a");
    let non_offline: Vec<_> = r
        .findings()
        .iter()
        .filter(|f| {
            !f.offline_detectable()
                && matches!(
                    f.severity(),
                    Severity::Critical | Severity::High | Severity::Medium
                )
        })
        .collect();
    assert!(
        non_offline.is_empty(),
        "non-offline findings: {non_offline:?}"
    );
}

// ── Scenario B ───────────────────────────────────────────────────────────────

#[test]
fn scenario_b_f1_semver_golang_base() {
    let (_t, r) = audit_scenario("scenario_b");
    assert!(r.findings().iter().any(|f| {
        f.severity() == Severity::High
            && f.dimension() == 5
            && f.current_value().contains("golang:1.22-alpine")
    }));
}

#[test]
fn scenario_b_f2_no_user_instruction() {
    let (_t, r) = audit_scenario("scenario_b");
    assert!(
        r.findings()
            .iter()
            .any(|f| f.severity() == Severity::High && f.dimension() == 12)
    );
}

#[test]
fn scenario_b_f3_latest_tag_critical() {
    let (_t, r) = audit_scenario("scenario_b");
    assert!(r.findings().iter().any(|f| {
        f.severity() == Severity::Critical
            && f.dimension() == 5
            && f.current_value().contains(":latest")
    }));
}

#[test]
fn scenario_b_f4_static_aws_credentials() {
    let (_t, r) = audit_scenario("scenario_b");
    assert!(r.findings().iter().any(|f| {
        f.severity() == Severity::High
            && f.dimension() == 6
            && f.current_value().to_uppercase().contains("AWS")
    }));
}

#[test]
fn scenario_b_f5_mutable_replace_directive() {
    let (_t, r) = audit_scenario("scenario_b");
    assert!(r.findings().iter().any(|f| {
        f.severity() == Severity::Medium
            && f.dimension() == 11
            && f.current_value().to_lowercase().contains("replace")
    }));
}

#[test]
fn scenario_b_severity_distribution() {
    let (_t, r) = audit_scenario("scenario_b");
    assert!(count(&r, Severity::Critical) >= 1);
    assert!(count(&r, Severity::High) >= 2);
}

#[test]
fn scenario_b_minimum_critical_and_high() {
    let (_t, r) = audit_scenario("scenario_b");
    assert!(count(&r, Severity::Critical) >= 1 && count(&r, Severity::High) >= 1);
}

// ── Scenario C ───────────────────────────────────────────────────────────────

#[test]
fn scenario_c_f1_no_nuget_lock_file() {
    let (_t, r) = audit_scenario("scenario_c");
    assert!(
        r.findings()
            .iter()
            .any(|f| { f.severity() == Severity::High && f.dimension() == 7 && f.line() == 0 })
    );
}

#[test]
fn scenario_c_f2_dependency_confusion_risk() {
    let (_t, r) = audit_scenario("scenario_c");
    assert!(r.findings().iter().any(|f| {
        f.severity() == Severity::High && f.dimension() == 7 && f.file().contains("NuGet.Config")
    }));
}

#[test]
fn scenario_c_f3_cargo_lock_in_gitignore() {
    let (_t, r) = audit_scenario("scenario_c");
    assert!(r.findings().iter().any(|f| {
        f.severity() == Severity::Medium
            && f.dimension() == 9
            && f.current_value().contains("Cargo.lock")
    }));
}

#[test]
fn scenario_c_f4_checkout_unpinned() {
    let (_t, r) = audit_scenario("scenario_c");
    assert!(r.findings().iter().any(|f| {
        f.severity() == Severity::High
            && f.dimension() == 1
            && f.current_value().contains("checkout@v4")
    }));
}

#[test]
fn scenario_c_f5_no_permissions_key() {
    let (_t, r) = audit_scenario("scenario_c");
    assert!(
        r.findings()
            .iter()
            .any(|f| f.severity() == Severity::Medium && f.dimension() == 2)
    );
}

#[test]
fn scenario_c_f6_rust_toolchain_mutable_ref() {
    let (_t, r) = audit_scenario("scenario_c");
    assert!(r.findings().iter().any(|f| {
        f.severity() == Severity::High
            && f.dimension() == 1
            && f.current_value().contains("rust-toolchain@stable")
    }));
}

#[test]
fn scenario_c_no_critical_findings() {
    let (_t, r) = audit_scenario("scenario_c");
    assert_eq!(count(&r, Severity::Critical), 0);
}

#[test]
fn scenario_c_slsa_assessment_present() {
    let (_t, r) = audit_scenario("scenario_c");
    assert!(r.render_report().contains("SLSA"));
}

#[test]
fn scenario_c_slsa_reports_l1_with_blockers() {
    let (_t, r) = audit_scenario("scenario_c");
    let report = r.render_report();
    assert!(report.contains("L1"));
    assert!(report.contains("L2") || report.to_lowercase().contains("provenance"));
}

#[test]
fn scenario_c_slsa_flags_unpinned_action_refs() {
    let (_t, r) = audit_scenario("scenario_c");
    let slsa = r.get_slsa_assessment().expect("slsa present");
    assert!(!slsa.action_refs_sha_pinned, "should detect unpinned refs");
}

#[test]
fn scenario_c_severity_distribution() {
    let (_t, r) = audit_scenario("scenario_c");
    assert_eq!(count(&r, Severity::Critical), 0);
    assert!(count(&r, Severity::High) >= 3);
    assert!(count(&r, Severity::Medium) >= 1);
}

#[test]
fn scenario_c_total_findings_at_least_4() {
    let (_t, r) = audit_scenario("scenario_c");
    assert!(r.findings().len() >= 4);
}

// Ensures fixtures directory is wired for `Path` import lint-cleanliness.
#[allow(dead_code)]
fn _fixtures_exist() {
    let _ = Path::new("tests/fixtures");
}
