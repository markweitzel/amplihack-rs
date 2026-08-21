//! Version querying, sanitization, and npm subprocess execution.

use crate::util::{run_output_with_timeout, strip_ansi};
use std::process::Command;
use std::time::Duration;

/// Subprocess timeout for each npm command.
const NPM_TIMEOUT: Duration = Duration::from_secs(3);

// `get_installed_version` (`npm list -g --depth=0 --json`) was removed by issue
// #1266 along with its JSON parser. It answered under npm's AMBIENT prefix —
// not the `--prefix` amplihack installs to, and not the binary it launches — so
// on any host where those differ it reported the version of a file nobody was
// going to run. That mismatch drove a full reinstall on every single launch,
// and it made the advisory update notice tell users to upgrade to a version
// they were already running. Both callers now read
// `amplihack_utils::launch_target::resolve(tool)`, which answers about the
// binary that will actually be executed. Do not reintroduce it.

/// Query the latest published version of an npm package from the registry.
///
/// Runs: `npm show <pkg> version`
/// Returns the first token on stdout as the version string.
///
/// Returns `None` if npm is unavailable, times out, or the package is unknown.
pub fn get_latest_version(pkg: &str) -> Option<String> {
    // SEC-WS3: pkg is always a &'static str from npm_package_for_tool().
    // It is never a user-controlled runtime string.
    let output = run_npm_with_timeout(&["show", pkg, "version"], NPM_TIMEOUT)?;
    let version = output.split_whitespace().next()?.to_string();
    if version.is_empty() {
        return None;
    }
    Some(version)
}

/// Strip all characters from `s` that are not safe for semver display.
///
/// Strips ANSI escape sequences, then applies an allowlist of `[a-zA-Z0-9.\-+]`.
/// Prevents terminal injection from a malicious npm registry response.
///
/// ```rust
/// # use amplihack_cli::tool_update_check::sanitize_version;
/// assert_eq!(sanitize_version("1.2.3"), "1.2.3");
/// assert_eq!(sanitize_version("\x1b[31m1.2.3\x1b[0m"), "1.2.3");
/// assert_eq!(sanitize_version("1.2.3\n"), "1.2.3");
/// ```
pub fn sanitize_version(s: &str) -> String {
    // Strip ANSI escape sequences first (ANSI codes contain alphanumeric chars
    // that would otherwise survive the allowlist filter and corrupt output).
    let stripped = strip_ansi(s);

    // Allowlist filter — keep only semver-safe characters: [a-zA-Z0-9.\-+]
    // Pre-allocate to the stripped length; result is always ≤ input length.
    let mut result = String::with_capacity(stripped.len());
    result.extend(
        stripped
            .chars()
            .filter(|c| c.is_ascii_alphanumeric() || *c == '.' || *c == '-' || *c == '+'),
    );
    result
}

/// Run `npm <args>` with a hard timeout, returning stdout on success.
///
/// Uses the shared child-killing timeout helper so a hung or malicious npm
/// binary on PATH cannot keep running after this function returns.
///
/// Returns `None` if:
/// - `npm` is not found on PATH
/// - The process does not complete within `timeout`
/// - The process exits with a non-zero status
/// - stdout is not valid UTF-8
pub fn run_npm_with_timeout(args: &[&str], timeout: Duration) -> Option<String> {
    let mut cmd = Command::new("npm");
    cmd.args(args);
    let output = run_output_with_timeout(cmd, timeout).ok()?;
    if !output.status.success() {
        return None;
    }
    String::from_utf8(output.stdout).ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(target_os = "linux")]
    #[test]
    fn run_npm_with_timeout_terminates_child_after_timeout() {
        let _guard = crate::test_support::home_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let temp = tempfile::tempdir().unwrap();
        let sentinel = temp.path().join("npm-still-ran");
        let fake_npm = temp.path().join("npm");
        std::fs::write(
            &fake_npm,
            "#!/bin/sh\n\
             /bin/sleep 0.2\n\
             printf 'late' > \"$AMPLIHACK_NPM_SENTINEL\"\n\
             printf '1.2.3\\n'\n",
        )
        .unwrap();
        let mut perms = std::fs::metadata(&fake_npm).unwrap().permissions();
        std::os::unix::fs::PermissionsExt::set_mode(&mut perms, 0o755);
        std::fs::set_permissions(&fake_npm, perms).unwrap();

        let previous_path = std::env::var_os("PATH");
        let previous_sentinel = std::env::var_os("AMPLIHACK_NPM_SENTINEL");
        unsafe {
            std::env::set_var("PATH", temp.path());
            std::env::set_var("AMPLIHACK_NPM_SENTINEL", &sentinel);
        }

        let result = run_npm_with_timeout(&["--version"], Duration::from_millis(10));
        std::thread::sleep(Duration::from_millis(350));

        match previous_path {
            Some(value) => unsafe { std::env::set_var("PATH", value) },
            None => unsafe { std::env::remove_var("PATH") },
        }
        match previous_sentinel {
            Some(value) => unsafe { std::env::set_var("AMPLIHACK_NPM_SENTINEL", value) },
            None => unsafe { std::env::remove_var("AMPLIHACK_NPM_SENTINEL") },
        }

        assert!(result.is_none(), "timeout should return no npm output");
        assert!(
            !sentinel.exists(),
            "timed-out npm subprocess must be terminated, not left running in a background thread"
        );
    }
}
