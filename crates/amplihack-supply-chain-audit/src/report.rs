//! Report generation — `SlsaAssessment` and `AuditReport`.

use crate::schema::{Finding, Severity};
use std::collections::BTreeMap;

/// SLSA compliance assessment table (L0 / L1 / L2 logic).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SlsaAssessment {
    pub build_is_scripted: bool,
    pub runs_on_hosted_ci: bool,
    pub provenance_generated: bool,
    pub action_refs_sha_pinned: bool,
}

impl SlsaAssessment {
    pub fn new(
        build_is_scripted: bool,
        runs_on_hosted_ci: bool,
        provenance_generated: bool,
        action_refs_sha_pinned: bool,
    ) -> Self {
        Self {
            build_is_scripted,
            runs_on_hosted_ci,
            provenance_generated,
            action_refs_sha_pinned,
        }
    }

    /// Current SLSA level string: `"L0"`, `"L1"`, or `"L2"`.
    pub fn current_level(&self) -> String {
        unimplemented!("SlsaAssessment::current_level is not yet implemented")
    }

    /// Render the SLSA compliance table (plus blockers) as markdown.
    pub fn render(&self) -> String {
        unimplemented!("SlsaAssessment::render is not yet implemented")
    }
}

/// A complete audit report with all five required sections.
#[derive(Debug, Clone)]
pub struct AuditReport {
    findings: Vec<Finding>,
    active_dims: Vec<u32>,
    skipped_dims: Vec<u32>,
    skip_reasons: Vec<(u32, String)>,
    root: String,
    scope: Vec<String>,
    slsa: Option<SlsaAssessment>,
    tool_status: BTreeMap<String, String>,
    advisory_messages: Vec<String>,
    generate_sbom: bool,
    handoffs: BTreeMap<String, String>,
}

impl AuditReport {
    /// Begin building a report from findings and dimension status.
    pub fn builder(
        findings: Vec<Finding>,
        active_dims: Vec<u32>,
        skipped_dims: Vec<u32>,
    ) -> AuditReportBuilder {
        AuditReportBuilder {
            findings,
            active_dims,
            skipped_dims,
            skip_reasons: Vec::new(),
            root: String::new(),
            scope: Vec::new(),
            slsa: None,
            tool_status: BTreeMap::new(),
            advisory_messages: Vec::new(),
            generate_sbom: false,
            handoffs: BTreeMap::new(),
        }
    }

    pub fn findings(&self) -> &[Finding] {
        &self.findings
    }

    /// Render the full markdown report.
    pub fn render(&self) -> String {
        unimplemented!("AuditReport::render is not yet implemented")
    }

    /// Render only the header, summary table, and dimension status.
    pub fn render_summary_only(&self) -> String {
        unimplemented!("AuditReport::render_summary_only is not yet implemented")
    }

    pub fn get_handoff(&self, skill: &str) -> Option<String> {
        self.handoffs.get(skill).cloned()
    }

    pub fn available_handoffs(&self) -> Vec<String> {
        self.handoffs.keys().cloned().collect()
    }

    pub fn get_advisory_messages(&self) -> &[String] {
        &self.advisory_messages
    }

    pub fn slsa(&self) -> Option<&SlsaAssessment> {
        self.slsa.as_ref()
    }

    // Field accessors used by the JSON renderer / audit orchestration.
    pub fn active_dims(&self) -> &[u32] {
        &self.active_dims
    }
    pub fn skipped_dims(&self) -> &[u32] {
        &self.skipped_dims
    }
    pub fn scope(&self) -> &[String] {
        &self.scope
    }
    pub fn root(&self) -> &str {
        &self.root
    }
    pub fn tool_status(&self) -> &BTreeMap<String, String> {
        &self.tool_status
    }
    pub fn generate_sbom(&self) -> bool {
        self.generate_sbom
    }
    pub fn skip_reasons(&self) -> &[(u32, String)] {
        &self.skip_reasons
    }

    /// Count findings by severity.
    pub fn severity_counts(&self) -> BTreeMap<Severity, usize> {
        unimplemented!("AuditReport::severity_counts is not yet implemented")
    }
}

/// Builder for [`AuditReport`].
#[derive(Debug, Clone)]
pub struct AuditReportBuilder {
    findings: Vec<Finding>,
    active_dims: Vec<u32>,
    skipped_dims: Vec<u32>,
    skip_reasons: Vec<(u32, String)>,
    root: String,
    scope: Vec<String>,
    slsa: Option<SlsaAssessment>,
    tool_status: BTreeMap<String, String>,
    advisory_messages: Vec<String>,
    generate_sbom: bool,
    handoffs: BTreeMap<String, String>,
}

impl AuditReportBuilder {
    pub fn root(mut self, root: impl Into<String>) -> Self {
        self.root = root.into();
        self
    }
    pub fn scope(mut self, scope: Vec<String>) -> Self {
        self.scope = scope;
        self
    }
    pub fn skip_reasons(mut self, reasons: Vec<(u32, String)>) -> Self {
        self.skip_reasons = reasons;
        self
    }
    pub fn slsa(mut self, slsa: Option<SlsaAssessment>) -> Self {
        self.slsa = slsa;
        self
    }
    pub fn tool_status(mut self, status: BTreeMap<String, String>) -> Self {
        self.tool_status = status;
        self
    }
    pub fn advisory_messages(mut self, messages: Vec<String>) -> Self {
        self.advisory_messages = messages;
        self
    }
    pub fn generate_sbom(mut self, v: bool) -> Self {
        self.generate_sbom = v;
        self
    }
    pub fn handoffs(mut self, handoffs: BTreeMap<String, String>) -> Self {
        self.handoffs = handoffs;
        self
    }

    pub fn build(self) -> AuditReport {
        AuditReport {
            findings: self.findings,
            active_dims: self.active_dims,
            skipped_dims: self.skipped_dims,
            skip_reasons: self.skip_reasons,
            root: self.root,
            scope: self.scope,
            slsa: self.slsa,
            tool_status: self.tool_status,
            advisory_messages: self.advisory_messages,
            generate_sbom: self.generate_sbom,
            handoffs: self.handoffs,
        }
    }
}
