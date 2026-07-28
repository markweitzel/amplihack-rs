//! Inter-skill handoff message construction for the audit report.

use crate::schema::{Finding, Severity};
use std::collections::BTreeMap;

/// Build inter-skill handoff messages from findings and active dimensions.
pub(super) fn build_handoffs(
    findings: &[Finding],
    active_dims: &[u32],
) -> BTreeMap<String, String> {
    let mut handoffs: BTreeMap<String, String> = BTreeMap::new();

    build_dependency_resolver_handoff(findings, &mut handoffs);
    build_pre_commit_handoff(findings, active_dims, &mut handoffs);
    build_cybersecurity_handoff(findings, &mut handoffs);
    build_silent_degradation_handoff(findings, &mut handoffs);

    handoffs
}

fn eco_for_dim(dim: u32) -> &'static str {
    match dim {
        7 => "dotnet",
        8 => "python",
        9 => "rust",
        10 => "node",
        11 => "go",
        _ => "unknown",
    }
}

fn validation_cmd(eco: &str) -> Option<&'static str> {
    match eco {
        "python" => Some("pip install --require-hashes -r requirements.txt"),
        "node" => Some("npm ci"),
        "dotnet" => Some("dotnet restore --locked-mode"),
        "rust" => Some("cargo build --locked"),
        "go" => Some("go mod verify"),
        _ => None,
    }
}

fn build_dependency_resolver_handoff(findings: &[Finding], out: &mut BTreeMap<String, String>) {
    let lock: Vec<&Finding> = findings
        .iter()
        .filter(|f| matches!(f.dimension(), 7..=11))
        .collect();
    if lock.is_empty() {
        return;
    }
    let mut ecosystems: Vec<String> = lock
        .iter()
        .map(|f| eco_for_dim(f.dimension()).to_string())
        .collect();
    ecosystems.sort();
    ecosystems.dedup();
    let finding_ids: Vec<&str> = lock.iter().map(|f| f.id()).collect();
    let ci_cmds: Vec<&str> = ecosystems
        .iter()
        .filter_map(|e| validation_cmd(e))
        .collect();
    out.insert(
        "dependency-resolver".to_string(),
        format!(
            "Ecosystems with lock file issues: {}\n\
             Finding IDs: {}\n\
             CI validation commands: {}\n\
             Please resolve lock file issues and run validation commands to verify.",
            ecosystems.join(", "),
            finding_ids.join(", "),
            ci_cmds.join("; ")
        ),
    );
}

fn build_pre_commit_handoff(
    findings: &[Finding],
    active_dims: &[u32],
    out: &mut BTreeMap<String, String>,
) {
    let has = |dims: &[u32]| dims.iter().any(|d| active_dims.contains(d));
    let mut hooks: Vec<&str> = Vec::new();
    if has(&[1, 2, 3]) {
        hooks.push("zizmor (Dims 1-3: GHA security)");
        hooks.push("actionlint (Dim 2: workflow syntax)");
    }
    if has(&[3, 6]) {
        hooks.push("detect-secrets (Dims 3+6: secret scanning)");
    }
    if has(&[5, 12]) {
        hooks.push("hadolint (Dims 5+12: Dockerfile linting)");
    }
    if active_dims.contains(&9) {
        hooks.push("cargo-audit (Dim 9: Cargo vulnerability scan)");
    }
    if active_dims.contains(&11) {
        hooks.push("go mod verify (Dim 11: Go module integrity)");
    }

    if hooks.is_empty() && active_dims.is_empty() {
        return;
    }
    let prevented: Vec<&str> = findings
        .iter()
        .filter(|f| matches!(f.dimension(), 1..=3) && f.offline_detectable())
        .map(|f| f.id())
        .collect();
    let mut dims_sorted: Vec<u32> = active_dims.to_vec();
    dims_sorted.sort_unstable();
    let dims_str = dims_sorted
        .iter()
        .map(|d| d.to_string())
        .collect::<Vec<_>>()
        .join(", ");
    out.insert(
        "pre-commit-manager".to_string(),
        format!(
            "Hooks to install: {}\n\
             Active ecosystems: {}\n\
             Findings this would have prevented: {}",
            if hooks.is_empty() {
                "none detected".to_string()
            } else {
                hooks.join(", ")
            },
            dims_str,
            if prevented.is_empty() {
                "none".to_string()
            } else {
                prevented.join(", ")
            }
        ),
    );
}

fn build_cybersecurity_handoff(findings: &[Finding], out: &mut BTreeMap<String, String>) {
    let count = |sev: Severity| findings.iter().filter(|f| f.severity() == sev).count();
    let runtime: Vec<&Finding> = findings
        .iter()
        .filter(|f| matches!(f.severity(), Severity::Critical | Severity::High))
        .collect();
    if runtime.is_empty() {
        return;
    }
    let ids: Vec<&str> = runtime.iter().take(10).map(|f| f.id()).collect();
    out.insert(
        "cybersecurity-analyst".to_string(),
        format!(
            "Supply chain posture summary:\n\
             Critical: {}\nHigh: {}\nMedium: {}\nInfo: {}\n\
             Finding IDs requiring runtime review: {}",
            count(Severity::Critical),
            count(Severity::High),
            count(Severity::Medium),
            count(Severity::Info),
            ids.join(", ")
        ),
    );
}

fn build_silent_degradation_handoff(findings: &[Finding], out: &mut BTreeMap<String, String>) {
    let degraded: Vec<&Finding> = findings
        .iter()
        .filter(|f| {
            f.current_value()
                .to_lowercase()
                .contains("continue-on-error")
        })
        .collect();
    if degraded.is_empty() {
        return;
    }
    let lines: Vec<String> = degraded
        .iter()
        .map(|f| format!("- {}: {}:{}", f.id(), f.file(), f.line()))
        .collect();
    out.insert(
        "silent-degradation-audit".to_string(),
        format!(
            "Security gates with continue-on-error detected:\n{}\n\
             These steps should be enforcing security gates. \
             continue-on-error allows pipeline to pass despite security failures.",
            lines.join("\n")
        ),
    );
}
