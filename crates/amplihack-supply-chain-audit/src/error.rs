//! Named error conditions for supply-chain-audit.
//!
//! Mirrors the upstream Python `errors.py` contract: each named error keeps a
//! stable `error_code` and a `Display` message that begins with the verbatim
//! `CODE:` prefix so downstream tooling and tests can match on it.

use std::fmt;

/// Convenience result alias for the crate.
pub type Result<T> = std::result::Result<T, SupplyChainAuditError>;

/// The strict scope allowlist, rendered into `INVALID_SCOPE` messages.
pub const VALID_SCOPES: [&str; 9] = [
    "all",
    "containers",
    "credentials",
    "dotnet",
    "gha",
    "go",
    "node",
    "python",
    "rust",
];

/// All error conditions raised by the audit engine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SupplyChainAuditError {
    /// `INVALID_SCOPE`: an unrecognized `--scope` value rejected before any file reads.
    InvalidScope { scope: String },
    /// `PATH_TRAVERSAL`: `..`, null byte, or an escaping symlink in the audit root.
    PathTraversal { path: String },
    /// `TOOL_TIMEOUT`: an external tool exceeded its timeout; audit continues degraded.
    ToolTimeout { tool: String, timeout: u64 },
    /// `ACCEPTED_RISKS_OVERFLOW`: `.supply-chain-accepted-risks.yml` exceeds 64 KiB.
    AcceptedRisksOverflow { size: u64 },
    /// `XPIA_ESCALATION`: prompt-injection markers in scanned content (advisory; retained
    /// for API compatibility — no longer raised by [`crate::run_audit`]).
    XpiaEscalation { file: String },
    /// Generic field/schema validation failure (equivalent to Python `ValueError`).
    Validation(String),
    /// I/O failure surfaced from path validation and file access.
    Io(String),
}

impl SupplyChainAuditError {
    /// The stable machine-readable error code for this condition.
    pub fn error_code(&self) -> &'static str {
        match self {
            Self::InvalidScope { .. } => "INVALID_SCOPE",
            Self::PathTraversal { .. } => "PATH_TRAVERSAL",
            Self::ToolTimeout { .. } => "TOOL_TIMEOUT",
            Self::AcceptedRisksOverflow { .. } => "ACCEPTED_RISKS_OVERFLOW",
            Self::XpiaEscalation { .. } => "XPIA_ESCALATION",
            Self::Validation(_) => "VALIDATION",
            Self::Io(_) => "IO",
        }
    }
}

impl fmt::Display for SupplyChainAuditError {
    fn fmt(&self, _f: &mut fmt::Formatter<'_>) -> fmt::Result {
        unimplemented!("SupplyChainAuditError::fmt is not yet implemented")
    }
}

impl std::error::Error for SupplyChainAuditError {}
