//! Finding schema — severity, finding IDs, and the finding record contract.
//!
//! This is the scaffold surface for TDD. Behaviour is intentionally
//! `unimplemented!()` so the ported tests compile and fail until the checker
//! logic is written.

use crate::error::{Result, SupplyChainAuditError};
use std::fmt;
use std::str::FromStr;

/// Severity band for a finding. Ordered Critical → High → Medium → Info.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Severity {
    Critical,
    High,
    Medium,
    Info,
}

impl Severity {
    /// Canonical mixed-case label (`"Critical"`, `"High"`, `"Medium"`, `"Info"`).
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::Critical => "Critical",
            Self::High => "High",
            Self::Medium => "Medium",
            Self::Info => "Info",
        }
    }

    /// Uppercase prefix used in finding IDs (`"CRITICAL"`, ...).
    pub fn prefix(&self) -> &'static str {
        match self {
            Self::Critical => "CRITICAL",
            Self::High => "HIGH",
            Self::Medium => "MEDIUM",
            Self::Info => "INFO",
        }
    }

    /// Sort rank: Critical = 0 (most severe) … Info = 3.
    pub fn rank(&self) -> u8 {
        match self {
            Self::Critical => 0,
            Self::High => 1,
            Self::Medium => 2,
            Self::Info => 3,
        }
    }

    /// All severities in descending order.
    pub fn all() -> [Severity; 4] {
        [
            Severity::Critical,
            Severity::High,
            Severity::Medium,
            Severity::Info,
        ]
    }
}

impl fmt::Display for Severity {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Severity {
    type Err = SupplyChainAuditError;

    fn from_str(_s: &str) -> Result<Self> {
        unimplemented!("Severity::from_str is not yet implemented")
    }
}

/// A validated finding identifier in `{SEVERITY}-{NNN}` format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingId {
    raw: String,
}

impl FindingId {
    /// Parse and validate a finding ID. Rejects wildcards, lowercase prefixes,
    /// non-3-digit sequences, and unknown severity prefixes.
    pub fn parse(_id: &str) -> Result<FindingId> {
        unimplemented!("FindingId::parse is not yet implemented")
    }

    /// The severity encoded in the ID prefix.
    pub fn severity(&self) -> Severity {
        unimplemented!("FindingId::severity is not yet implemented")
    }

    /// The numeric sequence portion (e.g. `42` for `HIGH-042`).
    pub fn sequence(&self) -> u32 {
        unimplemented!("FindingId::sequence is not yet implemented")
    }
}

impl fmt::Display for FindingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

/// A single supply-chain security finding.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finding {
    id: String,
    dimension: u32,
    severity: Severity,
    file: String,
    line: u32,
    current_value: String,
    expected_value: String,
    rationale: String,
    offline_detectable: bool,
    tool_required: Option<String>,
    contains_secret: bool,
    fix_url: Option<String>,
    accepted_risk: bool,
}

impl Finding {
    /// Start building a finding with the nine required fields. `line` is `i64`
    /// so the builder can validate and reject negative values.
    #[allow(clippy::too_many_arguments)]
    pub fn builder(
        id: impl Into<String>,
        dimension: u32,
        severity: Severity,
        file: impl Into<String>,
        line: i64,
        current_value: impl Into<String>,
        expected_value: impl Into<String>,
        rationale: impl Into<String>,
        offline_detectable: bool,
    ) -> FindingBuilder {
        FindingBuilder {
            id: id.into(),
            dimension,
            severity,
            file: file.into(),
            line,
            current_value: current_value.into(),
            expected_value: expected_value.into(),
            rationale: rationale.into(),
            offline_detectable,
            tool_required: None,
            contains_secret: false,
            fix_url: None,
            accepted_risk: false,
        }
    }

    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn dimension(&self) -> u32 {
        self.dimension
    }
    pub fn severity(&self) -> Severity {
        self.severity
    }
    pub fn file(&self) -> &str {
        &self.file
    }
    pub fn line(&self) -> u32 {
        self.line
    }
    pub fn current_value(&self) -> &str {
        &self.current_value
    }
    pub fn expected_value(&self) -> &str {
        &self.expected_value
    }
    pub fn rationale(&self) -> &str {
        &self.rationale
    }
    pub fn offline_detectable(&self) -> bool {
        self.offline_detectable
    }
    pub fn tool_required(&self) -> Option<&str> {
        self.tool_required.as_deref()
    }
    pub fn contains_secret(&self) -> bool {
        self.contains_secret
    }
    pub fn fix_url(&self) -> Option<&str> {
        self.fix_url.as_deref()
    }
    pub fn accepted_risk(&self) -> bool {
        self.accepted_risk
    }

    /// Render the finding as markdown, redacting secrets and sanitizing XPIA.
    pub fn render(&self) -> String {
        unimplemented!("Finding::render is not yet implemented")
    }
}

/// Builder for [`Finding`] that validates on [`FindingBuilder::build`].
#[derive(Debug, Clone)]
pub struct FindingBuilder {
    id: String,
    dimension: u32,
    severity: Severity,
    file: String,
    line: i64,
    current_value: String,
    expected_value: String,
    rationale: String,
    offline_detectable: bool,
    tool_required: Option<String>,
    contains_secret: bool,
    fix_url: Option<String>,
    accepted_risk: bool,
}

impl FindingBuilder {
    pub fn tool_required(mut self, tool: impl Into<String>) -> Self {
        self.tool_required = Some(tool.into());
        self
    }
    pub fn tool_required_opt(mut self, tool: Option<String>) -> Self {
        self.tool_required = tool;
        self
    }
    pub fn contains_secret(mut self, v: bool) -> Self {
        self.contains_secret = v;
        self
    }
    pub fn fix_url(mut self, url: impl Into<String>) -> Self {
        self.fix_url = Some(url.into());
        self
    }
    pub fn accepted_risk(mut self, v: bool) -> Self {
        self.accepted_risk = v;
        self
    }

    /// Validate all fields and construct the [`Finding`], or return a
    /// [`SupplyChainAuditError::Validation`] describing the first failure.
    pub fn build(self) -> Result<Finding> {
        unimplemented!("FindingBuilder::build is not yet implemented")
    }
}

/// Check a slice of findings for duplicate IDs.
pub fn validate_finding(_findings: &[Finding]) -> Result<()> {
    unimplemented!("validate_finding is not yet implemented")
}

/// Strip XPIA prompt-injection patterns from display text, replacing each with
/// `[XPIA-REDACTED]`.
pub fn sanitize_for_display(_text: &str) -> String {
    unimplemented!("sanitize_for_display is not yet implemented")
}
