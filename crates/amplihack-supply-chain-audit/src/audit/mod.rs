//! Audit orchestration — `AuditConfig`, `run_audit`, and `AuditResult`.

use crate::checkers;
use crate::detector::detect_ecosystems;
use crate::error::Result;
use crate::external_tools::check_tool_availability;
use crate::report::{AuditReport, SlsaAssessment};
use crate::schema::{Finding, Severity, xpia_advisory};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

mod handoffs;
mod support;

use handoffs::build_handoffs;
use support::{
    apply_accepted_risks, assign_ids_global, build_advisory_messages, build_slsa_assessment,
    filter_by_min_severity, load_accepted_risks, load_workflow_files, validate_path,
};

/// Configuration for an audit run.
#[derive(Debug, Clone)]
pub struct AuditConfig {
    root: PathBuf,
    scope: String,
    min_severity: Severity,
    generate_sbom: bool,
}

impl AuditConfig {
    /// A config for `root` with default scope `"all"` and min-severity `Info`.
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Self {
            root: root.into(),
            scope: "all".to_string(),
            min_severity: Severity::Info,
            generate_sbom: false,
        }
    }

    pub fn with_scope(mut self, scope: impl Into<String>) -> Self {
        self.scope = scope.into();
        self
    }

    pub fn with_min_severity(mut self, severity: Severity) -> Self {
        self.min_severity = severity;
        self
    }

    pub fn with_generate_sbom(mut self, v: bool) -> Self {
        self.generate_sbom = v;
        self
    }

    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn scope(&self) -> &str {
        &self.scope
    }
    pub fn min_severity(&self) -> Severity {
        self.min_severity
    }
    pub fn generate_sbom(&self) -> bool {
        self.generate_sbom
    }
}

/// The result of a complete audit run.
#[derive(Debug, Clone)]
pub struct AuditResult {
    report: AuditReport,
    active_dimensions: Vec<u32>,
    skipped_dimensions: Vec<u32>,
    slsa: Option<SlsaAssessment>,
}

impl AuditResult {
    /// Internal constructor used by [`run_audit`].
    pub(crate) fn new(
        report: AuditReport,
        active_dimensions: Vec<u32>,
        skipped_dimensions: Vec<u32>,
        slsa: Option<SlsaAssessment>,
    ) -> Self {
        Self {
            report,
            active_dimensions,
            skipped_dimensions,
            slsa,
        }
    }

    pub fn findings(&self) -> &[Finding] {
        self.report.findings()
    }

    pub fn active_dimensions(&self) -> &[u32] {
        &self.active_dimensions
    }

    pub fn skipped_dimensions(&self) -> &[u32] {
        &self.skipped_dimensions
    }

    /// Render the full markdown report.
    pub fn render_report(&self) -> String {
        self.report.render()
    }

    /// Render only the summary + dimension status (no findings list).
    pub fn render_report_summary_only(&self) -> String {
        self.report.render_summary_only()
    }

    /// Alias for [`AuditResult::render_report`].
    pub fn to_markdown(&self) -> String {
        self.report.render()
    }

    /// Serialize the result to JSON (schema-parity with upstream `to_dict`).
    pub fn to_json(&self) -> String {
        let findings: Vec<serde_json::Value> =
            self.findings().iter().map(finding_to_json).collect();

        let mut by_severity = serde_json::Map::new();
        for sev in Severity::all() {
            let count = self
                .findings()
                .iter()
                .filter(|f| f.severity() == sev)
                .count();
            by_severity.insert(sev.as_str().to_string(), serde_json::json!(count));
        }

        let slsa = match &self.slsa {
            Some(s) => serde_json::json!({
                "build_is_scripted": s.build_is_scripted,
                "runs_on_hosted_ci": s.runs_on_hosted_ci,
                "provenance_generated": s.provenance_generated,
                "action_refs_sha_pinned": s.action_refs_sha_pinned,
                "current_level": s.current_level(),
            }),
            None => serde_json::Value::Null,
        };

        let value = serde_json::json!({
            "findings": findings,
            "active_dimensions": self.active_dimensions,
            "skipped_dimensions": self.skipped_dimensions,
            "slsa": slsa,
            "advisories": self.get_advisory_messages(),
            "summary": {
                "total": self.findings().len(),
                "by_severity": by_severity,
            },
        });

        serde_json::to_string_pretty(&value).unwrap_or_else(|_| "{}".to_string())
    }

    /// The SLSA assessment, if GHA dimensions were active.
    pub fn get_slsa_assessment(&self) -> Option<SlsaAssessment> {
        self.slsa.clone()
    }

    /// Advisory messages (degraded tools, skipped files, XPIA notices).
    pub fn get_advisory_messages(&self) -> &[String] {
        self.report.get_advisory_messages()
    }

    /// Inter-skill handoff skill names available.
    pub fn available_handoffs(&self) -> Vec<String> {
        self.report.available_handoffs()
    }

    /// A specific inter-skill handoff message, if present.
    pub fn get_handoff(&self, skill: &str) -> Option<String> {
        self.report.get_handoff(skill)
    }
}

fn finding_to_json(f: &Finding) -> serde_json::Value {
    serde_json::json!({
        "id": f.id(),
        "dimension": f.dimension(),
        "severity": f.severity().as_str(),
        "file": f.file(),
        "line": f.line(),
        "current_value": f.current_value(),
        "expected_value": f.expected_value(),
        "rationale": f.rationale(),
        "offline_detectable": f.offline_detectable(),
        "tool_required": f.tool_required(),
        "contains_secret": f.contains_secret(),
        "fix_url": f.fix_url(),
        "accepted_risk": f.accepted_risk(),
    })
}

/// The dimension → checker registry, mirroring upstream `dim_checkers`.
fn dim_checker(dim: u32) -> Option<fn(&Path) -> Vec<Finding>> {
    match dim {
        1 => Some(checkers::check_action_sha_pinning),
        2 => Some(checkers::check_workflow_permissions),
        3 => Some(checkers::check_secret_exposure),
        4 => Some(checkers::check_cache_poisoning),
        5 => Some(checkers::check_container_image_pinning),
        6 => Some(checkers::check_credential_hygiene),
        7 => Some(checkers::check_nuget_lock),
        8 => Some(checkers::check_python_integrity),
        9 => Some(checkers::check_cargo_supply_chain),
        10 => Some(checkers::check_node_integrity),
        11 => Some(checkers::check_go_module_integrity),
        12 => Some(checkers::check_docker_build_chain),
        _ => None,
    }
}

/// Run XPIA scan (workflow files only) plus every active dimension checker.
fn run_all_checkers(active_dims: &[u32], root: &Path) -> (Vec<Finding>, Vec<String>) {
    let mut findings: Vec<Finding> = Vec::new();
    let mut xpia_advisories: Vec<String> = Vec::new();

    // XPIA check on workflow files — only when a GHA/secret dimension is active.
    if active_dims.iter().any(|d| matches!(d, 1 | 2 | 3 | 4 | 6)) {
        for (path, content) in load_workflow_files(root) {
            let rel = crate::checkers::utils::relative_path(root, &path);
            if let Some(advisory) = xpia_advisory(&content, &rel) {
                xpia_advisories.push(advisory);
            }
        }
    }

    for &dim in active_dims {
        if let Some(checker) = dim_checker(dim) {
            findings.extend(checker(root));
        }
    }

    (findings, xpia_advisories)
}

/// Run a complete supply chain audit.
///
/// Enforces, in order: `PATH_TRAVERSAL` rejection, `ACCEPTED_RISKS_OVERFLOW`,
/// then `INVALID_SCOPE` validation before running any dimension checkers.
pub fn run_audit(config: &AuditConfig) -> Result<AuditResult> {
    // ── Invariant 1: PATH_TRAVERSAL check (before scope validation) ──────────
    let root = validate_path(config.root())?;

    // ── Invariant 3: Accepted-risks size + wildcard load (before scope) ──────
    let accepted_risks = load_accepted_risks(&root)?;

    // ── Invariant 2 + Step 1: detect ecosystems (validates scope) ────────────
    let ecosystems = detect_ecosystems(&root, config.scope())?;
    let active_dims: Vec<u32> = ecosystems.active_dimensions().to_vec();
    let skipped_dims: Vec<u32> = ecosystems.skipped_dimensions().to_vec();
    let skip_reasons: Vec<(u32, String)> = ecosystems.skip_reasons().to_vec();

    // ── Step 2: tool availability ────────────────────────────────────────────
    let tool_status: BTreeMap<String, String> = check_tool_availability();

    // ── Step 3: run checkers (XPIA scan + dimension checkers) ────────────────
    let (all_findings, mut xpia_advisories) = run_all_checkers(&active_dims, &root);

    // ── Step 4: assign stable per-report IDs ─────────────────────────────────
    let all_findings = assign_ids_global(all_findings);

    // ── Step 5: apply accepted-risks suppressions ────────────────────────────
    let all_findings = apply_accepted_risks(all_findings, &accepted_risks);

    // ── Step 6: apply min-severity filter ────────────────────────────────────
    let filtered = filter_by_min_severity(all_findings, config.min_severity());

    // ── Step 7: build SLSA assessment ────────────────────────────────────────
    let slsa = if active_dims.iter().any(|d| matches!(d, 1..=4)) {
        let (assessment, warnings) = build_slsa_assessment(&root, &filtered);
        xpia_advisories.extend(warnings);
        Some(assessment)
    } else {
        None
    };

    // ── Step 8: inter-skill handoffs ─────────────────────────────────────────
    let handoffs = build_handoffs(&filtered, &active_dims);

    // ── Step 9: advisory messages ────────────────────────────────────────────
    let mut advisory_messages = build_advisory_messages(&tool_status);
    advisory_messages.extend(xpia_advisories);
    if config.generate_sbom() {
        advisory_messages.push(sbom_advisory());
    }

    let scope_list: Vec<String> = config
        .scope()
        .split(',')
        .map(|s| s.trim().to_string())
        .collect();

    let report = AuditReport::builder(filtered, active_dims.clone(), skipped_dims.clone())
        .root(root.to_string_lossy().to_string())
        .scope(scope_list)
        .skip_reasons(skip_reasons)
        .slsa(slsa.clone())
        .tool_status(tool_status)
        .advisory_messages(advisory_messages)
        .generate_sbom(config.generate_sbom())
        .handoffs(handoffs)
        .build();

    Ok(AuditResult::new(report, active_dims, skipped_dims, slsa))
}

/// The SBOM write advisory (emitted when `--generate-sbom` is requested).
fn sbom_advisory() -> String {
    "SBOM Advisory: Writing sbom.spdx.json to the repository exposes your full \
     dependency tree publicly. Add to .gitignore if not intended for version control. \
     Prefer uploading as a workflow artifact instead of committing."
        .to_string()
}
