//! External tool integration layer (`gh`, `crane`, `syft`, `grype`, `cosign`).
//!
//! All clients use argument arrays only (never shell string interpolation),
//! enforce per-tool timeouts, and degrade gracefully (returning `None`, never
//! panicking) when a tool is missing or times out.

use std::collections::BTreeMap;
use std::time::Instant;

/// The five optional external tools, in stable order.
pub const TOOL_NAMES: [&str; 5] = ["gh", "crane", "syft", "grype", "cosign"];

/// Documented per-tool timeout (seconds). Returns `None` for unknown tools.
pub fn tool_timeout(name: &str) -> Option<u64> {
    match name {
        "gh" => Some(15),
        "crane" => Some(20),
        "syft" => Some(120),
        "grype" => Some(60),
        "cosign" => Some(30),
        _ => None,
    }
}

/// A lightweight in-process circuit breaker.
///
/// Opens after `failure_threshold` consecutive failures; permits a half-open
/// probe once `reset_timeout_secs` have elapsed since the last failure.
#[derive(Debug)]
pub struct CircuitBreaker {
    failure_threshold: u32,
    reset_timeout_secs: u64,
    failure_count: u32,
    is_open: bool,
    last_failure: Option<Instant>,
}

impl Default for CircuitBreaker {
    fn default() -> Self {
        Self::with_config(3, 60)
    }
}

impl CircuitBreaker {
    /// A breaker with the default threshold (3) and reset window (60s).
    pub fn new() -> Self {
        Self::default()
    }

    /// A breaker with an explicit failure threshold and reset window.
    pub fn with_config(failure_threshold: u32, reset_timeout_secs: u64) -> Self {
        Self {
            failure_threshold,
            reset_timeout_secs,
            failure_count: 0,
            is_open: false,
            last_failure: None,
        }
    }

    /// True when the circuit is open (the tool is considered unavailable).
    pub fn is_open(&self) -> bool {
        if !self.is_open {
            return false;
        }
        !matches!(self.last_failure, Some(t) if t.elapsed().as_secs() >= self.reset_timeout_secs)
    }

    /// Record a successful call — resets the breaker.
    pub fn record_success(&mut self) {
        self.failure_count = 0;
        self.is_open = false;
    }

    /// Record a failed call — opens the breaker at the threshold.
    pub fn record_failure(&mut self) {
        self.failure_count += 1;
        self.last_failure = Some(Instant::now());
        if self.failure_count >= self.failure_threshold {
            self.is_open = true;
        }
    }

    /// Force the breaker closed.
    pub fn reset(&mut self) {
        self.failure_count = 0;
        self.is_open = false;
        self.last_failure = None;
    }

    // Accessors so callers/tests can inspect configuration.
    pub fn failure_threshold(&self) -> u32 {
        self.failure_threshold
    }
    pub fn reset_timeout_secs(&self) -> u64 {
        self.reset_timeout_secs
    }
}

/// A configured adapter for one external CLI tool.
#[derive(Debug, Clone)]
pub struct ToolClient {
    name: String,
    timeout: u64,
}

impl ToolClient {
    /// Construct a client for a named tool, or `None` if the tool is unknown.
    pub fn new(name: &str) -> Option<ToolClient> {
        tool_timeout(name).map(|timeout| ToolClient {
            name: name.to_string(),
            timeout,
        })
    }

    pub fn name(&self) -> &str {
        &self.name
    }
    pub fn timeout(&self) -> u64 {
        self.timeout
    }

    /// True when the tool is resolvable on the operator's `PATH`.
    pub fn is_available(&self) -> bool {
        which::which(&self.name).is_ok()
    }
}

/// Install metadata for a missing tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingTool {
    pub name: String,
    pub description: String,
    pub install_options: Vec<String>,
}

/// Availability status for every tool: `name -> status string`.
///
/// Status strings contain `"available"` or `"unavailable"`.
pub fn check_tool_availability() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for name in TOOL_NAMES {
        let timeout = tool_timeout(name).unwrap_or(0);
        let status = if which::which(name).is_ok() {
            format!("available (timeout: {timeout}s)")
        } else {
            "unavailable (not found in PATH)".to_string()
        };
        map.insert(name.to_string(), status);
    }
    map
}

/// Missing tools with install instructions. Empty when all are present.
pub fn check_missing_tools() -> Vec<MissingTool> {
    TOOL_NAMES
        .iter()
        .filter(|name| which::which(name).is_err())
        .filter_map(|name| install_options(name))
        .collect()
}

/// Install metadata (description + options) for a tool, if known.
pub fn install_options(name: &str) -> Option<MissingTool> {
    let (description, options): (&str, &[&str]) = match name {
        "gh" => (
            "GitHub CLI — required for provenance/attestation checks",
            &[
                "linux_apt: sudo apt install -y gh",
                "linux_dnf: sudo dnf install -y gh",
                "macos: brew install gh",
            ],
        ),
        "crane" => (
            "go-containerregistry crane — required for container image inspection",
            &["go_install: go install github.com/google/go-containerregistry/cmd/crane@latest"],
        ),
        "syft" => (
            "Anchore syft — required for SBOM generation",
            &[
                "script: curl -sSfL https://raw.githubusercontent.com/anchore/syft/main/install.sh | sh -s -- -b /usr/local/bin",
            ],
        ),
        "grype" => (
            "Anchore grype — required for vulnerability scanning",
            &[
                "script: curl -sSfL https://raw.githubusercontent.com/anchore/grype/main/install.sh | sh -s -- -b /usr/local/bin",
            ],
        ),
        "cosign" => (
            "Sigstore cosign — required for signature verification",
            &["go_install: go install github.com/sigstore/cosign/v2/cmd/cosign@latest"],
        ),
        _ => return None,
    };
    Some(MissingTool {
        name: name.to_string(),
        description: description.to_string(),
        install_options: options.iter().map(|s| s.to_string()).collect(),
    })
}
