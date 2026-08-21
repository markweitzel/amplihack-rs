//! Claude CLI binary detection, installation, and version checking.
//!
//! Ported from `amplihack/utils/claude_cli.py`. Provides helpers to locate
//! the `claude` CLI binary, validate that it works, ensure it is installed
//! (via npm), and compare the installed version against the latest published
//! version.
//!
//! # Status: thin wrapper, no production callers
//!
//! Binary resolution used to be implemented independently here — a private
//! PATH search, a private validation probe, a private npm prefix, and a
//! private `npm install` — making this the **fourth** divergent resolver in
//! the tree (issue #1266). All of that now delegates to
//! [`crate::launch_target`], the single health-gated resolver.
//!
//! The public signatures below are preserved *solely* so this crate's
//! `no_run` doctests keep compiling. They have no production callers. Do not
//! mistake a surviving signature for a supported API: new code should call
//! [`crate::launch_target::resolve`] directly, and installation belongs to
//! `amplihack-cli`'s `bootstrap`, which owns the platform-package two-step
//! that makes a claude install an actual install.

use crate::launch_target::{self, Health};
use crate::process::ProcessManager;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;
use thiserror::Error;

// Errors

/// Errors produced by Claude CLI operations.
#[derive(Debug, Error)]
pub enum ClaudeCliError {
    /// A subprocess operation failed.
    #[error("process error: {0}")]
    Process(#[from] crate::process::ProcessError),

    /// npm is not installed — required for auto-installation.
    #[error("npm is not installed; install Node.js first")]
    NpmNotFound,

    /// The installation command exited with a non-zero status.
    #[error("npm install failed (exit {code:?}): {stderr}")]
    InstallFailed {
        /// Exit code from npm, if available.
        code: Option<i32>,
        /// Captured stderr from the install command.
        stderr: String,
    },

    /// The installed binary could not be validated.
    #[error("claude binary at {path} failed validation: {reason}")]
    ValidationFailed {
        /// Path to the binary that was tested.
        path: String,
        /// Human-readable reason.
        reason: String,
    },
}

// Version status

/// Comparison of the installed Claude CLI version against the latest
/// published version.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum VersionStatus {
    /// Installed version is up to date.
    Current(String),
    /// A newer version is available.
    UpdateAvailable {
        /// Currently installed version.
        current: String,
        /// Latest published version.
        latest: String,
    },
    /// Could not determine version information.
    Unknown,
}

// Constants

/// npm package name for Claude Code.
const CLAUDE_NPM_PACKAGE: &str = "@anthropic-ai/claude-code";

/// Default timeout for version-check subprocesses.
const VERSION_TIMEOUT: Duration = Duration::from_secs(10);

/// Regex for extracting a semantic version from a string.
static SEMVER_RE: LazyLock<regex::Regex> =
    LazyLock::new(|| regex::Regex::new(r"(\d+\.\d+\.\d+)").expect("semver regex"));

// Binary detection

/// Find the claude CLI binary path.
///
/// Search order and health gating are owned by
/// [`launch_target::resolve`]: explicit env override, then `$PATH`, then
/// amplihack's known install directories.
///
/// Returns `None` when no candidate is found **or** when every candidate
/// found fails the health gate.
///
/// Note: the agent-binary identifier (claude/copilot/codex/amplifier) is
/// resolved separately via [`crate::agent_binary::resolve`]; that value is a
/// short name, not a filesystem path, and is not consulted here.
///
/// # Examples
///
/// ```no_run
/// use amplihack_utils::claude_cli::get_claude_cli_path;
///
/// if let Some(path) = get_claude_cli_path() {
///     println!("claude binary: {}", path.display());
/// }
/// ```
pub fn get_claude_cli_path() -> Option<PathBuf> {
    // Delegates to the single resolver. Note the behaviour change this
    // inherits, and which is the point of issue #1266: a candidate that fails
    // the health gate is never returned. The previous implementation happily
    // handed back a ~500-byte stub, which the caller then exec'd.
    launch_target::resolve("claude")
        .selected
        .map(|candidate| candidate.path)
}

// Installation

/// Ensure the claude CLI is installed and return its path.
///
/// If the binary is already present and passes validation, its path is
/// returned immediately. Otherwise an npm user-local install is attempted.
///
/// # Errors
///
/// Returns [`ClaudeCliError`] if npm is not available, the install command
/// fails, or the installed binary cannot be validated.
///
/// # Examples
///
/// ```no_run
/// use amplihack_utils::claude_cli::ensure_claude_cli;
///
/// let path = ensure_claude_cli().expect("claude should be installable");
/// println!("claude ready at {}", path.display());
/// ```
pub fn ensure_claude_cli() -> Result<PathBuf, ClaudeCliError> {
    if let Some(path) = get_claude_cli_path() {
        return Ok(path);
    }

    // This module deliberately no longer carries its own `npm install`. The
    // one it used to run suppressed the postinstall that materializes
    // `@anthropic-ai/claude-code`'s native binary, producing exactly the stub
    // this function was then asked to validate. Installation lives in
    // `amplihack-cli`'s `bootstrap`, which owns the platform-package step that
    // makes the install real.
    let rejected = launch_target::render_rejections(&launch_target::resolve("claude"));
    Err(ClaudeCliError::ValidationFailed {
        path: "claude".into(),
        reason: format!(
            "no working claude binary was found. Candidates considered:\n{rejected}\
             Run `amplihack claude` to install one, or install manually:\n  \
             npm install -g @anthropic-ai/claude-code"
        ),
    })
}

// Version checking

/// Extract a semantic version string from command output.
///
/// Looks for the first `\d+\.\d+\.\d+` match in `text`.
fn parse_semver(text: &str) -> Option<String> {
    SEMVER_RE.captures(text).map(|c| c[1].to_string())
}

/// Get the installed version of the claude binary at `binary`.
///
/// Reads through the health gate rather than probing independently, so a stub
/// can never report a version.
fn get_installed_version(binary: &Path) -> Option<String> {
    match launch_target::probe_health(binary) {
        Health::Working { semver, .. } => semver,
        Health::Broken(_) => None,
    }
}

/// Query npm for the latest published version of the Claude Code package.
fn get_latest_published_version() -> Option<String> {
    let mgr = ProcessManager::new();
    let result = mgr
        .run_command(
            &["npm", "view", CLAUDE_NPM_PACKAGE, "version"],
            Some(VERSION_TIMEOUT),
            None,
            None,
        )
        .ok()?;
    if !result.success() {
        return None;
    }
    parse_semver(&result.stdout)
}

/// Compare two semantic version strings.
///
/// Returns `true` when `latest` is strictly newer than `current`.
fn is_newer(current: &str, latest: &str) -> bool {
    let parse = |v: &str| -> Option<(u64, u64, u64)> {
        let v = v.strip_prefix('v').unwrap_or(v);
        let parts: Vec<&str> = v.split('.').collect();
        if parts.len() != 3 {
            return None;
        }
        Some((
            parts[0].parse().ok()?,
            parts[1].parse().ok()?,
            parts[2].parse().ok()?,
        ))
    };
    match (parse(current), parse(latest)) {
        (Some(c), Some(l)) => l > c,
        _ => false,
    }
}

/// Check whether the installed claude version is up to date.
///
/// Queries the installed binary for its version and compares against the
/// latest version published on npm.
///
/// # Errors
///
/// Returns [`ClaudeCliError::Process`] if subprocess execution fails.
///
/// # Examples
///
/// ```no_run
/// use amplihack_utils::claude_cli::{check_claude_version, VersionStatus};
/// use std::path::Path;
///
/// match check_claude_version(Path::new("/usr/local/bin/claude")) {
///     Ok(VersionStatus::Current(v)) => println!("up to date: {v}"),
///     Ok(VersionStatus::UpdateAvailable { current, latest }) => {
///         println!("update available: {current} → {latest}");
///     }
///     Ok(VersionStatus::Unknown) => println!("could not determine version"),
///     Err(e) => eprintln!("error: {e}"),
/// }
/// ```
pub fn check_claude_version(binary: &Path) -> Result<VersionStatus, ClaudeCliError> {
    let current = match get_installed_version(binary) {
        Some(v) => v,
        None => return Ok(VersionStatus::Unknown),
    };

    let latest = match get_latest_published_version() {
        Some(v) => v,
        None => {
            // Cannot determine latest — assume current is fine.
            return Ok(VersionStatus::Current(current));
        }
    };

    if is_newer(&current, &latest) {
        Ok(VersionStatus::UpdateAvailable { current, latest })
    } else {
        Ok(VersionStatus::Current(current))
    }
}

// Tests

#[cfg(test)]
#[path = "tests/claude_cli_tests.rs"]
mod tests;
