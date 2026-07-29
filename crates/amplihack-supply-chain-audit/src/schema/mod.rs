//! Finding schema — severity, finding IDs, and the finding record contract.

use crate::error::{Result, SupplyChainAuditError};
use once_cell::sync::Lazy;
use regex::Regex;
use std::fmt;
use std::str::FromStr;

mod finding;
pub use finding::{Finding, FindingBuilder};

/// Tools permitted in a finding's `tool_required` field (mirrors upstream
/// `VALID_TOOLS`; `None` is always allowed).
pub const VALID_TOOLS: [&str; 11] = [
    "crane",
    "gh",
    "syft",
    "grype",
    "cosign",
    "actionlint",
    "zizmor",
    "detect-secrets",
    "cargo-audit",
    "go-mod-verify",
    "hadolint",
];

// ── XPIA sanitization (defense-in-depth) ────────────────────────────────────
// The `regex` crate has no look-around, so the bare-`SYSTEM:` directive pattern
// (which upstream expresses with a negative look-behind) is handled separately
// by [`xpia_system_directive`].
static XPIA_PATTERNS: Lazy<Vec<(Regex, &'static str)>> = Lazy::new(|| {
    let sources = [
        r"(?i)</?\s*(?:system|user|assistant|human)\s*>",
        r"(?i)</s(?:ystem|ys)>",
        r"(?i)ignore\s+(?:previous\s+)?instructions",
        r"(?i)you\s+are\s+now\s+(?:dan|an?\s+ai|a\s+different)",
        r"(?i)new\s+instructions\s*:",
        r"(?i)disregard\s+(?:previous|all|your)",
        r"(?i)jailbreak",
    ];
    sources
        .iter()
        .map(|s| (Regex::new(s).expect("valid XPIA regex"), *s))
        .collect()
});

// Matches `SYSTEM:` case-insensitively; the preceding-character guard is applied
// manually to emulate the `(?<![a-zA-Z0-9_-])` look-behind.
static XPIA_SYSTEM: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)system\s*:").expect("valid SYSTEM regex"));

const XPIA_SYSTEM_SRC: &str = r"(?<![a-zA-Z0-9_-])SYSTEM\s*:";

fn is_boundary_prefix(prev: Option<char>) -> bool {
    match prev {
        None => true,
        Some(c) => !(c.is_ascii_alphanumeric() || c == '_' || c == '-'),
    }
}

/// Locate a bare `SYSTEM:` directive not preceded by `[A-Za-z0-9_-]`.
/// Returns the byte range of the match if present.
fn xpia_system_directive(text: &str) -> Option<(usize, usize)> {
    for m in XPIA_SYSTEM.find_iter(text) {
        let prev = text[..m.start()].chars().next_back();
        if is_boundary_prefix(prev) {
            return Some((m.start(), m.end()));
        }
    }
    None
}

/// Severity band for a finding. Ordered Critical → High → Medium → Info.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
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

    fn from_str(s: &str) -> Result<Self> {
        match s {
            "Critical" => Ok(Severity::Critical),
            "High" => Ok(Severity::High),
            "Medium" => Ok(Severity::Medium),
            "Info" => Ok(Severity::Info),
            other => Err(SupplyChainAuditError::Validation(format!(
                "severity must be one of [\"Critical\", \"High\", \"Info\", \"Medium\"], got {other:?}"
            ))),
        }
    }
}

/// A validated finding identifier in `{SEVERITY}-{NNN}` format.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FindingId {
    raw: String,
    severity: Severity,
    sequence: u32,
}

static ID_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"^(CRITICAL|HIGH|MEDIUM|INFO)-(\d{3})$").expect("valid id regex"));

fn severity_from_prefix(prefix: &str) -> Option<Severity> {
    match prefix {
        "CRITICAL" => Some(Severity::Critical),
        "HIGH" => Some(Severity::High),
        "MEDIUM" => Some(Severity::Medium),
        "INFO" => Some(Severity::Info),
        _ => None,
    }
}

impl FindingId {
    /// Parse and validate a finding ID. Rejects wildcards, lowercase prefixes,
    /// non-3-digit sequences, and unknown severity prefixes.
    pub fn parse(id: &str) -> Result<FindingId> {
        let err = SupplyChainAuditError::Validation;
        if id.contains('*') {
            return Err(err(format!("wildcard not allowed in finding ID: '{id}'")));
        }
        if let Some(caps) = ID_PATTERN.captures(id) {
            let severity = severity_from_prefix(&caps[1]).expect("prefix validated by regex");
            let sequence: u32 = caps[2].parse().expect("3-digit sequence");
            return Ok(FindingId {
                raw: id.to_string(),
                severity,
                sequence,
            });
        }
        // Detailed diagnostics matching upstream error messages.
        if !id.contains('-') {
            return Err(err(format!(
                "Invalid finding ID '{id}': missing severity prefix. \
                 Expected format: CRITICAL-001, HIGH-042, MEDIUM-007, INFO-001"
            )));
        }
        let (prefix, seq) = id.split_once('-').expect("contains '-'");
        if severity_from_prefix(prefix).is_none() {
            if severity_from_prefix(&prefix.to_uppercase()).is_some() {
                return Err(err(format!(
                    "Invalid severity prefix '{prefix}' in finding ID '{id}': \
                     severity prefix must be uppercase (CRITICAL, HIGH, MEDIUM, INFO)"
                )));
            }
            return Err(err(format!(
                "Invalid severity prefix '{prefix}' in finding ID '{id}'. \
                 Must be one of: CRITICAL, HIGH, MEDIUM, INFO"
            )));
        }
        if seq.is_empty() {
            return Err(err(format!(
                "Invalid finding ID '{id}': missing sequence number"
            )));
        }
        if seq.len() != 3 || !seq.bytes().all(|b| b.is_ascii_digit()) {
            return Err(err(format!(
                "Invalid finding ID '{id}': sequence must be 3-digit zero-padded \
                 (e.g., 001, 042, 007). Got: '{seq}'"
            )));
        }
        Err(err(format!("Invalid finding ID '{id}'")))
    }

    /// The severity encoded in the ID prefix.
    pub fn severity(&self) -> Severity {
        self.severity
    }

    /// The numeric sequence portion (e.g. `42` for `HIGH-042`).
    pub fn sequence(&self) -> u32 {
        self.sequence
    }
}

impl fmt::Display for FindingId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.raw)
    }
}

/// Check a slice of findings for duplicate IDs.
pub fn validate_finding(findings: &[Finding]) -> Result<()> {
    let mut seen: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for f in findings {
        if !seen.insert(f.id()) {
            return Err(SupplyChainAuditError::Validation(format!(
                "duplicate finding id detected: '{}' appears more than once in the report",
                f.id()
            )));
        }
    }
    Ok(())
}

/// Strip XPIA prompt-injection patterns from display text, replacing each with
/// `[XPIA-REDACTED]`.
pub fn sanitize_for_display(text: &str) -> String {
    let mut result = text.to_string();
    for (pattern, _) in XPIA_PATTERNS.iter() {
        result = pattern.replace_all(&result, "[XPIA-REDACTED]").into_owned();
    }
    // Handle the look-behind-guarded SYSTEM: directive manually.
    while let Some((start, end)) = xpia_system_directive(&result) {
        result.replace_range(start..end, "[XPIA-REDACTED]");
    }
    result
}

/// Return an XPIA advisory string for `content` scanned as `file`, or `None`
/// when no prompt-injection markers are present. The advisory never echoes the
/// matched content — only the pattern source — per the XPIA safety invariant.
pub(crate) fn xpia_advisory(content: &str, file: &str) -> Option<String> {
    let matched = XPIA_PATTERNS
        .iter()
        .find(|(re, _)| re.is_match(content))
        .map(|(_, src)| *src)
        .or_else(|| xpia_system_directive(content).map(|_| XPIA_SYSTEM_SRC))?;
    Some(format!(
        "⚠️ XPIA DETECTED: '{file}' matches pattern ({matched}). File was audited \
         normally (checkers extract structured data, not raw content). Review the \
         file manually or escalate to xpia-defense skill."
    ))
}
