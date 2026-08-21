//! Claude CLI binary detection, installation, and version checking.
//!
//! Ported from `amplihack/utils/claude_cli.py`. What remains here is the
//! version-comparison layer.
//!
//! # What this module no longer does (issue #1266)
//!
//! It used to carry its own installer, running
//! `npm install -g --ignore-scripts @anthropic-ai/claude-code` independently of
//! `bootstrap.rs`. That is exactly the invocation that leaves the 500-byte
//! placeholder behind, so a second installer nobody called was a second door
//! into the same bug. It is deleted rather than rewired, along with the
//! `NpmNotFound` / `InstallFailed` / `ValidationFailed` error variants that
//! only it could construct — leaving them in a public enum would advertise
//! failure modes this module can no longer reach.
//!
//! Binary resolution is likewise delegated: [`get_claude_cli_path`] is a thin
//! wrapper over [`crate::launch_target::resolve`], which is the single place in
//! the repo permitted to answer "which claude binary".

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
/// One line of delegation to [`crate::launch_target::resolve`], on purpose. The
/// search order, the health gate, and the install decision all live there so
/// they cannot drift apart — which is precisely what happened before issue
/// #1266: the version check read `/usr/bin/claude`, the install wrote
/// `~/.npm-global/bin/claude`, and the exec ran `~/.local/bin/claude`, all in
/// one launch.
///
/// Returns `None` when no *healthy* claude binary exists. A binary that is
/// present but cannot report a version is not a result here — health is a
/// filter, never an annotation.
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
    crate::launch_target::resolve("claude")
        .target
        .map(|target| target.path)
}

// Version checking

/// Extract a semantic version string from command output.
///
/// Looks for the first `\d+\.\d+\.\d+` match in `text`.
fn parse_semver(text: &str) -> Option<String> {
    SEMVER_RE.captures(text).map(|c| c[1].to_string())
}

/// Get the installed version of the claude binary at `binary`.
fn get_installed_version(binary: &Path) -> Option<String> {
    let mgr = ProcessManager::new();
    let path_str = binary.to_str()?;
    let result = mgr
        .run_command(&[path_str, "--version"], Some(VERSION_TIMEOUT), None, None)
        .ok()?;
    if !result.success() {
        return None;
    }
    parse_semver(&result.stdout)
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
