//! The finding record contract — `Finding` and its validating builder.

use super::{FindingId, Severity, VALID_TOOLS, sanitize_for_display};
use crate::error::{Result, SupplyChainAuditError};

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

    /// The display-safe form of a value: fully redacted when the finding
    /// contains a secret, otherwise XPIA-sanitized. Both render paths share
    /// this so redaction can never diverge between them.
    pub(crate) fn display_value(&self, value: &str) -> String {
        if self.contains_secret {
            "<REDACTED>".to_string()
        } else {
            sanitize_for_display(value)
        }
    }

    /// Render the finding as markdown, redacting secrets and sanitizing XPIA.
    pub fn render(&self) -> String {
        let current = self.display_value(&self.current_value);
        let expected = self.display_value(&self.expected_value);
        let mut lines = vec![
            format!(
                "**Finding {}** (Dim {}) — **{}**",
                self.id, self.dimension, self.severity
            ),
            format!("**File**: `{}:{}`", self.file, self.line),
            format!("**Severity**: {}", self.severity),
            format!("**Current**: `{current}`"),
            format!("**Expected**: `{expected}`"),
            format!("**Why**: {}", sanitize_for_display(&self.rationale)),
        ];
        if self.accepted_risk {
            lines.push("_[ACCEPTED RISK — review date applies]_".to_string());
        }
        if let Some(url) = &self.fix_url {
            lines.push(format!("**Fix**: {}", sanitize_for_display(url)));
        }
        lines.join("\n")
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
        let err = SupplyChainAuditError::Validation;

        // ID format (also rejects wildcards).
        FindingId::parse(&self.id)?;

        // Dimension range.
        if self.dimension < 1 || self.dimension > 12 {
            return Err(err(format!(
                "dimension must be an integer 1-12, got {}",
                self.dimension
            )));
        }

        // File path: relative POSIX, no traversal, no null byte.
        let f = &self.file;
        if f.starts_with('/') {
            return Err(err(format!(
                "file must be a relative POSIX path, got absolute path: {f:?}"
            )));
        }
        if f.split('/').any(|seg| seg == "..") {
            return Err(err(format!("file contains path traversal '..': {f:?}")));
        }
        if f.contains('\u{0}') {
            return Err(err(format!("file contains null byte: {f:?}")));
        }

        // Line number.
        if self.line < 0 {
            return Err(err(format!(
                "line must be a non-negative integer, got {}",
                self.line
            )));
        }

        // Tool allowlist.
        if let Some(tool) = &self.tool_required
            && !VALID_TOOLS.contains(&tool.as_str())
        {
            return Err(err(format!(
                "tool_required '{tool}' not in approved list: {VALID_TOOLS:?}"
            )));
        }

        Ok(Finding {
            id: self.id,
            dimension: self.dimension,
            severity: self.severity,
            file: self.file,
            line: self.line as u32,
            current_value: self.current_value,
            expected_value: self.expected_value,
            rationale: self.rationale,
            offline_detectable: self.offline_detectable,
            tool_required: self.tool_required,
            contains_secret: self.contains_secret,
            fix_url: self.fix_url,
            accepted_risk: self.accepted_risk,
        })
    }
}
