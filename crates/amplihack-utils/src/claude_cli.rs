//! Claude CLI binary resolution.
//!
//! Ported from `amplihack/utils/claude_cli.py`. What remains here is a single
//! delegating accessor; everything this module used to do itself now lives in
//! [`crate::launch_target`].
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
//! It also used to carry its own version check: `check_claude_version`, backed
//! by a private `<binary> --version` probe and a private
//! `npm view @anthropic-ai/claude-code version` query. That is deleted too, and
//! for the same reason the installer was. It had no callers, but a dead
//! duplicate is not harmless — it was a second answer to "what version is
//! installed" and a second answer to "what version is published", competing
//! with [`crate::launch_target`] and `tool_update_check` respectively. Those
//! are the two questions whose disagreement *is* issue #1266. The surviving
//! implementations memoize (so the advisory notice and the install decision
//! cannot disagree), bound the subprocess, and sanitize registry output before
//! believing it; this copy did none of the three. Deleting it removes the
//! footgun rather than leaving it for whoever greps for "version check" next.
//! `ClaudeCliError` and `VersionStatus` went with it, having become
//! unconstructible.
//!
//! Binary resolution is likewise delegated: [`get_claude_cli_path`] is a thin
//! wrapper over [`crate::launch_target::resolve`], which is the single place in
//! the repo permitted to answer "which claude binary".

use std::path::PathBuf;

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
/// This is the module's entire remaining surface, and it is kept public
/// deliberately: it is the sanctioned way to ask "which claude", and it
/// answers by routing through the single resolver. The alternative to having
/// one correct public accessor is the next caller writing a second one.
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

// Tests

#[cfg(test)]
#[path = "tests/claude_cli_tests.rs"]
mod tests;
