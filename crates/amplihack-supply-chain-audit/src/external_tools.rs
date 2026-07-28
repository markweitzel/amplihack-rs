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
        unimplemented!("CircuitBreaker::is_open is not yet implemented")
    }

    /// Record a successful call — resets the breaker.
    pub fn record_success(&mut self) {
        unimplemented!("CircuitBreaker::record_success is not yet implemented")
    }

    /// Record a failed call — opens the breaker at the threshold.
    pub fn record_failure(&mut self) {
        unimplemented!("CircuitBreaker::record_failure is not yet implemented")
    }

    /// Force the breaker closed.
    pub fn reset(&mut self) {
        unimplemented!("CircuitBreaker::reset is not yet implemented")
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
        unimplemented!("ToolClient::is_available is not yet implemented")
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
    unimplemented!("check_tool_availability is not yet implemented")
}

/// Missing tools with install instructions. Empty when all are present.
pub fn check_missing_tools() -> Vec<MissingTool> {
    unimplemented!("check_missing_tools is not yet implemented")
}

/// Install metadata (description + options) for a tool, if known.
pub fn install_options(name: &str) -> Option<MissingTool> {
    unimplemented!("install_options is not yet implemented: {name}")
}
