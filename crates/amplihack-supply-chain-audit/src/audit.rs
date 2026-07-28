//! Audit orchestration — `AuditConfig`, `run_audit`, and `AuditResult`.

use crate::error::Result;
use crate::report::{AuditReport, SlsaAssessment};
use crate::schema::{Finding, Severity};
use std::path::{Path, PathBuf};

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
        unimplemented!("AuditResult::to_json is not yet implemented")
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

/// Run a complete supply chain audit.
///
/// Enforces, in order: `PATH_TRAVERSAL` rejection, `INVALID_SCOPE` validation,
/// then `ACCEPTED_RISKS_OVERFLOW` before running any dimension checkers.
pub fn run_audit(_config: &AuditConfig) -> Result<AuditResult> {
    unimplemented!("run_audit is not yet implemented")
}
