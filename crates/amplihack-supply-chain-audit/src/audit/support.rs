//! Orchestration helpers for [`super::run_audit`] — path validation,
//! accepted-risks handling, ID assignment, SLSA, and handoffs.

use crate::error::{Result, SupplyChainAuditError};
use crate::report::SlsaAssessment;
use crate::schema::{Finding, Severity};
use chrono::Local;
use std::collections::BTreeMap;
use std::path::{Component, Path, PathBuf};

const MAX_ACCEPTED_RISKS_SIZE: u64 = 64 * 1024;

const HOSTED_RUNNERS: [&str; 7] = [
    "ubuntu-latest",
    "ubuntu-22.04",
    "ubuntu-20.04",
    "windows-latest",
    "windows-2022",
    "macos-latest",
    "macos-13",
];

// ── Path validation (Invariant 1) ───────────────────────────────────────────

/// Validate the audit root: reject `..`, null bytes, and escaping symlinks,
/// then confirm the path exists and is a directory.
pub(super) fn validate_path(root: &Path) -> Result<PathBuf> {
    check_path_traversal(root)?;
    let display = root.to_string_lossy().to_string();
    if !root.exists() {
        return Err(SupplyChainAuditError::Validation(format!(
            "Audit path does not exist: '{display}'"
        )));
    }
    if !root.is_dir() {
        return Err(SupplyChainAuditError::Validation(format!(
            "Audit path is not a directory: '{display}'"
        )));
    }
    Ok(root.to_path_buf())
}

fn check_path_traversal(path: &Path) -> Result<()> {
    let display = path.to_string_lossy().to_string();
    let traversal = || SupplyChainAuditError::PathTraversal {
        path: display.clone(),
    };

    if display.contains('\u{0}') {
        return Err(traversal());
    }
    if path.components().any(|c| c == Component::ParentDir) {
        return Err(traversal());
    }
    if path.is_symlink() {
        let resolved = std::fs::canonicalize(path).ok();
        let parent_resolved = path.parent().and_then(|p| std::fs::canonicalize(p).ok());
        if let (Some(target), Some(parent)) = (resolved, parent_resolved)
            && target.strip_prefix(&parent).is_err()
        {
            return Err(traversal());
        }
    }
    Ok(())
}

// ── Accepted risks (Invariant 3) ────────────────────────────────────────────

type RiskEntry = BTreeMap<String, String>;

/// Load `.supply-chain-accepted-risks.yml`. Enforces the 64 KiB size cap and
/// rejects wildcard IDs. Returns an empty list when the file is absent.
pub(super) fn load_accepted_risks(root: &Path) -> Result<Vec<RiskEntry>> {
    let file = root.join(".supply-chain-accepted-risks.yml");
    if !file.exists() {
        return Ok(Vec::new());
    }
    let size = std::fs::metadata(&file).map(|m| m.len()).unwrap_or(0);
    if size > MAX_ACCEPTED_RISKS_SIZE {
        return Err(SupplyChainAuditError::AcceptedRisksOverflow { size });
    }
    let content = match std::fs::read_to_string(&file) {
        Ok(c) => c,
        Err(_) => return Ok(Vec::new()),
    };
    parse_accepted_risks(&content)
}

fn strip_quotes(s: &str) -> &str {
    s.trim_matches(|c| c == '\'' || c == '"')
}

fn parse_accepted_risks(content: &str) -> Result<Vec<RiskEntry>> {
    let mut entries: Vec<RiskEntry> = Vec::new();
    let mut current: Option<RiskEntry> = None;

    for line in content.lines() {
        let stripped = line.trim();
        if let Some(rest) = stripped.strip_prefix("- id:") {
            if let Some(entry) = current.take() {
                entries.push(entry);
            }
            let val = strip_quotes(rest.trim()).to_string();
            if val.contains('*') {
                return Err(SupplyChainAuditError::Validation(format!(
                    "wildcard not allowed in accepted-risks ID: '{val}'. \
                     Use explicit IDs only (e.g., HIGH-001, not HIGH-*)."
                )));
            }
            let mut entry = RiskEntry::new();
            entry.insert("id".to_string(), val);
            current = Some(entry);
        } else if stripped.contains(':')
            && let Some(entry) = current.as_mut()
        {
            let (key, val) = stripped.split_once(':').expect("contains ':'");
            let key = strip_quotes(key.trim()).to_string();
            let val = strip_quotes(val.trim()).to_string();
            entry.entry(key).or_insert(val);
        }
    }
    if let Some(entry) = current.take() {
        entries.push(entry);
    }
    Ok(entries)
}

/// Apply accepted-risks suppressions. Critical findings are never suppressed;
/// expired review dates retain original severity; valid entries demote to Info.
pub(super) fn apply_accepted_risks(findings: Vec<Finding>, risks: &[RiskEntry]) -> Vec<Finding> {
    if risks.is_empty() {
        return findings;
    }
    let today = Local::now().format("%Y-%m-%d").to_string();
    let by_id: BTreeMap<&str, &RiskEntry> = risks
        .iter()
        .filter_map(|r| r.get("id").map(|id| (id.as_str(), r)))
        .collect();

    findings
        .into_iter()
        .map(|f| {
            let risk = match by_id.get(f.id()) {
                Some(r) => *r,
                None => return f,
            };
            if f.severity() == Severity::Critical {
                return f;
            }
            let review_date = risk.get("review_date").map(String::as_str).unwrap_or("");
            if !review_date.is_empty() && review_date < today.as_str() {
                return f;
            }
            let accepted_by = risk.get("accepted_by").map(String::as_str).unwrap_or("");
            let mut parts: Vec<String> = Vec::new();
            if !accepted_by.is_empty() {
                parts.push(format!("accepted by {accepted_by}"));
            }
            if !review_date.is_empty() {
                parts.push(format!("review date: {review_date}"));
            }
            let suffix = if parts.is_empty() {
                String::new()
            } else {
                format!(" [Accepted risk — {}]", parts.join(", "))
            };
            rebuild(
                &f,
                f.id().to_string(),
                Severity::Info,
                format!("{}{suffix}", f.rationale()),
                true,
            )
        })
        .collect()
}

// ── ID assignment + filtering ───────────────────────────────────────────────

/// Assign stable `{SEVERITY}-{NNN}` IDs after sorting by
/// (severity, dimension, file, line).
pub(super) fn assign_ids_global(mut findings: Vec<Finding>) -> Vec<Finding> {
    findings.sort_by(|a, b| {
        a.severity()
            .rank()
            .cmp(&b.severity().rank())
            .then(a.dimension().cmp(&b.dimension()))
            .then(a.file().cmp(b.file()))
            .then(a.line().cmp(&b.line()))
    });
    let mut counters: [u32; 4] = [0; 4];
    findings
        .into_iter()
        .map(|f| {
            let idx = f.severity().rank() as usize;
            counters[idx] += 1;
            let id = format!("{}-{:03}", f.severity().prefix(), counters[idx]);
            rebuild(
                &f,
                id,
                f.severity(),
                f.rationale().to_string(),
                f.accepted_risk(),
            )
        })
        .collect()
}

/// Keep findings at or above the minimum severity threshold.
pub(super) fn filter_by_min_severity(findings: Vec<Finding>, min: Severity) -> Vec<Finding> {
    findings
        .into_iter()
        .filter(|f| f.severity().rank() <= min.rank())
        .collect()
}

/// Reconstruct a finding, overriding id, severity, rationale, and accepted flag.
fn rebuild(
    f: &Finding,
    id: String,
    severity: Severity,
    rationale: String,
    accepted_risk: bool,
) -> Finding {
    let mut builder = Finding::builder(
        id,
        f.dimension(),
        severity,
        f.file().to_string(),
        f.line() as i64,
        f.current_value().to_string(),
        f.expected_value().to_string(),
        rationale,
        f.offline_detectable(),
    )
    .contains_secret(f.contains_secret())
    .accepted_risk(accepted_risk);
    if let Some(tool) = f.tool_required() {
        builder = builder.tool_required(tool.to_string());
    }
    if let Some(url) = f.fix_url() {
        builder = builder.fix_url(url.to_string());
    }
    builder.build().expect("rebuilt finding is valid")
}

// ── Workflow loading + SLSA ─────────────────────────────────────────────────

/// Load `.github/workflows/*.{yml,yaml}` (sorted, lock files skipped).
pub(super) fn load_workflow_files(root: &Path) -> Vec<(PathBuf, String)> {
    crate::checkers::utils::load_workflows(root)
}

/// Build the SLSA assessment from workflow content and dim-1 findings.
pub(super) fn build_slsa_assessment(
    root: &Path,
    findings: &[Finding],
) -> (SlsaAssessment, Vec<String>) {
    let workflows = load_workflow_files(root);
    let build_is_scripted = !workflows.is_empty();
    let runs_on_hosted_ci = workflows
        .iter()
        .any(|(_, c)| HOSTED_RUNNERS.iter().any(|r| c.contains(r)));
    let provenance_generated = workflows
        .iter()
        .any(|(_, c)| c.contains("slsa-framework/slsa-github-generator"));
    let action_refs_sha_pinned = !findings
        .iter()
        .any(|f| f.dimension() == 1 && matches!(f.severity(), Severity::Critical | Severity::High));

    let assessment = SlsaAssessment::new(
        build_is_scripted,
        runs_on_hosted_ci,
        provenance_generated,
        action_refs_sha_pinned,
    );
    (assessment, Vec::new())
}

// ── Advisory messages ───────────────────────────────────────────────────────

/// Build advisory messages for degraded/unavailable tools.
pub(super) fn build_advisory_messages(tool_status: &BTreeMap<String, String>) -> Vec<String> {
    tool_status
        .iter()
        .filter(|(_, s)| s.contains("unavailable") || s.contains("TOOL_TIMEOUT"))
        .map(|(tool, _)| {
            format!(
                "Tool '{tool}' not available — running in degraded mode. \
                 Some checks that require this tool were skipped."
            )
        })
        .collect()
}
