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
    /// Retained for parity with the upstream `errors.py` contract and the
    /// `TOOL_TIMEOUT` report marker; not constructed while external tools are
    /// probed for availability only (no in-process command execution).
    ToolTimeout { tool: String, timeout: u64 },
    /// `ACCEPTED_RISKS_OVERFLOW`: `.supply-chain-accepted-risks.yml` exceeds 64 KiB.
    AcceptedRisksOverflow { size: u64 },
    /// `XPIA_ESCALATION`: prompt-injection markers in scanned content (advisory; retained
    /// for API compatibility — no longer raised by [`crate::run_audit`]).
    XpiaEscalation { file: String },
    /// Generic field/schema validation failure (equivalent to Python `ValueError`).
    Validation(String),
    /// I/O failure surfaced from path validation and file access. Retained for
    /// parity with the upstream contract; file-access errors currently map to
    /// [`Self::Validation`], so this variant is not presently constructed.
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

/// Format an integer with thousands separators (e.g. `65,536`), mirroring
/// Python's `{size:,}` formatting used in the overflow message.
fn with_thousands(n: u64) -> String {
    let digits = n.to_string();
    let bytes = digits.as_bytes();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    let first = bytes.len() % 3;
    for (i, b) in bytes.iter().enumerate() {
        if i != 0 && i >= first && (i - first).is_multiple_of(3) {
            out.push(',');
        }
        out.push(*b as char);
    }
    out
}

impl fmt::Display for SupplyChainAuditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidScope { scope } => {
                let valid = VALID_SCOPES.join(", ");
                write!(
                    f,
                    "INVALID_SCOPE: '{scope}' is not a recognized scope. \
                     Valid scopes: all, containers, credentials, dotnet, gha, go, node, python, rust. \
                     Full list: {valid}"
                )
            }
            Self::PathTraversal { path } => write!(
                f,
                "PATH_TRAVERSAL: Rejected audit root '{path}' — \
                 path contains '..' segments, null bytes, or a symlink that escapes the root."
            ),
            Self::ToolTimeout { tool, timeout } => write!(
                f,
                "TOOL_TIMEOUT: '{tool}' exceeded {timeout}s timeout; running in degraded mode"
            ),
            Self::AcceptedRisksOverflow { size } => write!(
                f,
                "ACCEPTED_RISKS_OVERFLOW: .supply-chain-accepted-risks.yml is {} bytes \
                 (max 65,536). Please split the file by year or archive resolved entries \
                 to a separate archive file.",
                with_thousands(*size)
            ),
            Self::XpiaEscalation { file } => write!(
                f,
                "XPIA_ESCALATION: Possible prompt injection markers detected in scanned file \
                 '{file}'. Audit aborted. Escalate to xpia-defense skill for investigation."
            ),
            Self::Validation(msg) => f.write_str(msg),
            Self::Io(msg) => f.write_str(msg),
        }
    }
}

impl std::error::Error for SupplyChainAuditError {}
