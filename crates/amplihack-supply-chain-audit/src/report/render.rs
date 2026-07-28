//! Markdown rendering for [`AuditReport`] and [`SlsaAssessment`].

use super::{AuditReport, SlsaAssessment, dim_name, dim_to_eco};
use crate::schema::{Severity, sanitize_for_display};
use std::collections::BTreeSet;

fn check(b: bool) -> &'static str {
    if b { "✅" } else { "❌" }
}

/// Render the SLSA compliance table plus blockers to the next level.
pub(super) fn render_slsa(s: &SlsaAssessment) -> String {
    let level = s.current_level();
    let mut lines = vec![
        "| Requirement | Status |".to_string(),
        "|-------------|--------|".to_string(),
        format!("| Build is scripted | {} |", check(s.build_is_scripted)),
        format!(
            "| Build runs on hosted CI | {} |",
            check(s.runs_on_hosted_ci)
        ),
        format!(
            "| Provenance generated | {} |",
            check(s.provenance_generated)
        ),
        format!(
            "| Action refs SHA-pinned | {} |",
            check(s.action_refs_sha_pinned)
        ),
        String::new(),
        format!("**Current SLSA Level: {level}**"),
    ];

    if level == "L0" {
        lines.push("\n**Blockers to L1:** Implement scripted build in CI.".to_string());
    } else if level == "L1" {
        let mut blockers = Vec::new();
        if !s.runs_on_hosted_ci {
            blockers.push("Move to hosted CI runner (e.g., ubuntu-latest)".to_string());
        }
        if !s.provenance_generated {
            blockers.push(
                "Add SLSA provenance generation (slsa-framework/slsa-github-generator)".to_string(),
            );
        }
        if !s.action_refs_sha_pinned {
            blockers.push("Pin all action refs to full SHA (Dim 1 findings)".to_string());
        }
        if !blockers.is_empty() {
            lines.push(format!("\n**Blockers to L2:** {}", blockers.join("; ")));
        }
        lines.push(
            "\n**Blockers to L3:** Add SLSA generator action with OIDC provenance signing."
                .to_string(),
        );
    }

    lines.join("\n")
}

fn render_summary_table(r: &AuditReport) -> Vec<String> {
    let counts = r.severity_counts();
    let total: usize = counts.values().sum();

    let mut lines = vec![
        "### Summary".to_string(),
        String::new(),
        "| Severity | Count |".to_string(),
        "|----------|-------|".to_string(),
    ];
    for sev in Severity::all() {
        lines.push(format!("| {} | {} |", sev.as_str(), counts[&sev]));
    }
    lines.push(format!("| **Total** | **{total}** |"));
    lines.push(String::new());

    let critical = counts[&Severity::Critical];
    let high = counts[&Severity::High];
    if critical == 0 && high == 0 {
        if total == 0 {
            lines.push("**Supply Chain Posture: Passing ✅** — No findings detected.".to_string());
        } else {
            lines.push(
                "**Supply Chain Posture: Passing ✅** — No Critical or High findings.".to_string(),
            );
        }
    } else {
        lines.push(format!(
            "**Supply Chain Posture: ⚠ Action Required** — {critical} Critical, {high} High findings."
        ));
    }
    lines.push(String::new());

    lines.extend([
        "#### Dimension Status".to_string(),
        String::new(),
        "| Dim | Name | Status | Reason |".to_string(),
        "|-----|------|--------|--------|".to_string(),
    ]);
    for dim in 1..=12u32 {
        let name = dim_name(dim);
        let (status, reason) = if r.active_dims().contains(&dim) {
            let n = r.findings().iter().filter(|f| f.dimension() == dim).count();
            let status = if n > 0 {
                format!("⚠ {n} finding(s)")
            } else {
                "✅ Checked".to_string()
            };
            (status, String::new())
        } else {
            let reason = r
                .skip_reasons()
                .iter()
                .find(|(d, _)| *d == dim)
                .map_or_else(String::new, |(_, s)| s.clone());
            ("⏭ Skipped".to_string(), reason)
        };
        lines.push(format!("| {dim} | {name} | {status} | {reason} |"));
    }
    lines.push(String::new());
    lines
}

fn render_findings_section(r: &AuditReport) -> Vec<String> {
    let mut lines = vec!["### Findings".to_string(), String::new()];

    if r.findings().is_empty() {
        lines.push("_No findings detected for audited dimensions._".to_string());
        lines.push(String::new());
        return lines;
    }

    let mut sorted: Vec<_> = r.findings().iter().collect();
    sorted.sort_by(|a, b| {
        (a.severity().rank(), a.dimension(), a.file(), a.line()).cmp(&(
            b.severity().rank(),
            b.dimension(),
            b.file(),
            b.line(),
        ))
    });

    for f in sorted {
        lines.push(format!(
            "#### {} — Dim {} — {}",
            f.id(),
            f.dimension(),
            f.severity()
        ));
        lines.push(String::new());
        lines.push(format!("**Severity**: {}", f.severity()));
        lines.push(format!("**File**: `{}:{}`", f.file(), f.line()));
        let current = f.display_value(f.current_value());
        lines.push(format!("**Current**: `{current}`"));
        let expected = f.display_value(f.expected_value());
        lines.push(format!("**Expected**: `{expected}`"));
        lines.push(format!("**Why**: {}", sanitize_for_display(f.rationale())));
        if f.accepted_risk() {
            lines.push("_[ACCEPTED RISK — review date applies]_".to_string());
        }
        if let Some(fix) = f.fix_url() {
            lines.push(format!("**Fix**: {}", sanitize_for_display(fix)));
        }
        if let Some(tool) = f.tool_required() {
            lines.push(format!("**Tool required**: {tool}"));
        }
        lines.push(String::new());
    }
    lines
}

fn render_slsa_section(r: &AuditReport) -> Vec<String> {
    let mut lines = vec!["### SLSA Readiness".to_string(), String::new()];
    if let Some(slsa) = r.slsa() {
        lines.push(render_slsa(slsa));
    } else {
        lines.push(
            "_SLSA assessment requires GHA scope. Run with --scope gha or --scope all._"
                .to_string(),
        );
    }
    lines.push(String::new());
    lines
}

fn render_next_steps(r: &AuditReport) -> Vec<String> {
    let mut lines = vec!["### Recommended Next Steps".to_string(), String::new()];
    let counts = r.severity_counts();

    if counts[&Severity::Critical] > 0 || counts[&Severity::High] > 0 {
        lines.push("**Priority 1 — Fix Critical and High findings immediately:**".to_string());
        let critical_high: Vec<_> = r
            .findings()
            .iter()
            .filter(|f| matches!(f.severity(), Severity::Critical | Severity::High))
            .collect();
        for f in critical_high.iter().take(3) {
            let snippet: String = f.rationale().chars().take(80).collect();
            lines.push(format!("- [ ] Fix `{}`: {snippet}...", f.id()));
        }
        lines.push(String::new());
    }

    let lock_dims = [7, 8, 9, 10, 11];
    let lock_findings: Vec<_> = r
        .findings()
        .iter()
        .filter(|f| lock_dims.contains(&f.dimension()))
        .collect();
    if !lock_findings.is_empty() {
        lines
            .push("**Delegate lock file remediation to `dependency-resolver` skill:**".to_string());
        let ecos: BTreeSet<String> = lock_findings
            .iter()
            .map(|f| dim_to_eco(f.dimension()))
            .collect();
        lines.push(format!(
            "- Affected ecosystems: {}",
            ecos.into_iter().collect::<Vec<_>>().join(", ")
        ));
        lines.push("- Run `/dependency-resolver` with the finding IDs above".to_string());
        lines.push(String::new());
    }

    lines.push("**Install pre-commit hooks to prevent regressions:**".to_string());
    lines.push("- Run `/pre-commit-manager` to install hooks for detected ecosystems".to_string());
    if r.findings().iter().any(|f| matches!(f.dimension(), 1..=3)) {
        lines.push(
            "  - zizmor / actionlint: GitHub Actions security linting (Dims 1-3)".to_string(),
        );
    }
    if r.findings().iter().any(|f| f.dimension() == 3) {
        lines.push("  - detect-secrets: Prevent secret commits (Dim 3+6)".to_string());
    }
    lines.push(String::new());

    let advisories = r.get_advisory_messages();
    if !advisories.is_empty() {
        lines.push("**Advisories:**".to_string());
        for msg in advisories {
            lines.push(format!("- {msg}"));
        }
        lines.push(String::new());
    }
    lines
}

/// Render the full markdown report (or summary-only when `summary_only`).
pub(super) fn render_report(r: &AuditReport, summary_only: bool) -> String {
    let today = chrono::Local::now().format("%Y-%m-%d").to_string();
    let scope = if r.scope().is_empty() {
        vec!["all".to_string()]
    } else {
        r.scope().to_vec()
    };
    let scope_str = scope.join(", ");

    let tool_avail = if r.tool_status().is_empty() {
        "gh: not checked, crane: not checked, syft: not checked, grype: not checked".to_string()
    } else {
        r.tool_status()
            .iter()
            .map(|(t, s)| format!("{t}: {s}"))
            .collect::<Vec<_>>()
            .join("; ")
    };

    let degraded: Vec<String> = r
        .tool_status()
        .iter()
        .filter(|(_, s)| {
            let l = s.to_lowercase();
            l.contains("unavailable") || l.contains("timeout")
        })
        .map(|(t, _)| t.clone())
        .collect();

    let root = if r.root().is_empty() { "." } else { r.root() };
    let mut skipped_sorted = r.skipped_dims().to_vec();
    skipped_sorted.sort_unstable();
    let skipped_str = if skipped_sorted.is_empty() {
        "none".to_string()
    } else {
        skipped_sorted
            .iter()
            .map(u32::to_string)
            .collect::<Vec<_>>()
            .join(", ")
    };

    let mut lines = vec![
        "## Supply Chain Audit Report".to_string(),
        String::new(),
        format!("**Date**: {today}"),
        format!("**Root**: {root}"),
        format!("**Scope**: {scope_str}"),
        format!("**Skipped**: Dims {skipped_str}"),
        format!("**Tool availability**: {tool_avail}"),
    ];
    if !degraded.is_empty() {
        lines.push(format!(
            "**⚠ Degraded mode**: {} unavailable — TOOL_TIMEOUT or not installed",
            degraded.join(", ")
        ));
    }
    lines.push(String::new());

    lines.extend(render_summary_table(r));
    if !summary_only {
        lines.extend(render_findings_section(r));
    }
    lines.extend(render_slsa_section(r));
    lines.extend(render_next_steps(r));

    lines.join("\n")
}
