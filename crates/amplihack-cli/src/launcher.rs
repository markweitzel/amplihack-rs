//! Process launcher with managed child and graceful shutdown.
//!
//! `ManagedChild` wraps `std::process::Child` with a bounded `Drop`
//! implementation that sends SIGTERM, waits up to 3 seconds, then
//! sends SIGKILL.

use anyhow::{Context, Result};
use std::process::{Child, Command, ExitStatus};
use std::time::{Duration, Instant};

/// A child process wrapper with graceful shutdown on drop.
///
/// On drop:
/// 1. If the child already exited, do nothing.
/// 2. Send SIGTERM (Unix) or kill (Windows).
/// 3. Wait up to 3 seconds for graceful exit.
/// 4. If still alive, SIGKILL + wait.
pub struct ManagedChild {
    child: Child,
}

impl ManagedChild {
    /// Spawn a command while preserving the caller's foreground TTY.
    pub fn spawn(mut cmd: Command) -> Result<Self> {
        let child = cmd.spawn().context("failed to spawn child process")?;
        tracing::debug!(pid = child.id(), "spawned managed child");
        Ok(Self { child })
    }

    /// Non-blocking check: has the child exited?
    pub fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        self.child
            .try_wait()
            .context("failed to check child status")
    }

    /// Blocking wait until child exits.
    pub fn wait(&mut self) -> Result<ExitStatus> {
        self.child.wait().context("failed to wait for child")
    }

    /// Get the child's PID.
    pub fn id(&self) -> u32 {
        self.child.id()
    }

    /// Explicitly terminate the child (SIGTERM → wait → SIGKILL).
    pub fn terminate(&mut self) {
        self.graceful_shutdown();
    }

    fn graceful_shutdown(&mut self) {
        // Already exited?
        if matches!(self.child.try_wait(), Ok(Some(_))) {
            return;
        }

        // Send SIGTERM
        #[cfg(unix)]
        {
            // SAFETY: We're sending a standard signal to a process we own.
            // The PID is valid because try_wait() above confirmed the child is still running.
            unsafe {
                libc::kill(i32::try_from(self.child.id()).unwrap_or(0), libc::SIGTERM);
            }
        }

        #[cfg(not(unix))]
        {
            let _ = self.child.kill();
        }

        // Wait up to 3 seconds for graceful exit
        let deadline = Instant::now() + Duration::from_secs(3);
        while Instant::now() < deadline {
            if matches!(self.child.try_wait(), Ok(Some(_))) {
                return;
            }
            std::thread::sleep(Duration::from_millis(50));
        }

        // Force kill
        tracing::warn!(
            pid = self.child.id(),
            "child did not exit gracefully, sending SIGKILL"
        );
        if let Err(e) = self.child.kill() {
            tracing::warn!("failed to kill child process: {e}");
        }
        if let Err(e) = self.child.wait() {
            tracing::warn!("failed to wait for child process: {e}");
        }
    }
}

impl Drop for ManagedChild {
    fn drop(&mut self) {
        self.graceful_shutdown();
    }
}

/// Translate a raw spawn failure into a message that names the real cause.
///
/// `ManagedChild::spawn` stays generic; the launch call site wraps its error
/// through this pure function. `report` is
/// [`amplihack_utils::launch_target::Resolution::rejection_report`] output for
/// the same tool, so the message describes what was actually tried.
///
/// The failure this exists to replace, observed on the dev VM 2026-08-21:
///
/// ```text
/// error: failed to spawn child process: Exec format error (os error 8)
/// ```
///
/// That names nothing real and sends the user hunting for a CPU-architecture
/// problem that does not exist. The actual cause was a 500-byte shell
/// placeholder being exec'd as if it were a native binary.
///
/// Carries paths, rejection reasons, and the remedy — never the environment,
/// never the full argv.
///
/// `package` is a parameter for the same reason `rejection_report` takes one:
/// this is the spawn-failure path for **every** tool, and its ENOEXEC prose
/// used to name `@anthropic-ai/claude-code` even when the thing that failed to
/// exec was copilot.
pub fn enrich_spawn_error(
    raw_os_error: Option<i32>,
    path: &std::path::Path,
    package: &str,
    report: &str,
) -> String {
    /// ENOEXEC. The kernel's answer when a small ASCII file with no shebang is
    /// handed to `execve` as if it were a native binary — i.e. the placeholder.
    const ENOEXEC: i32 = 8;

    let cause = match raw_os_error {
        Some(ENOEXEC) => format!(
            "The file is not a runnable program. This is the placeholder that \
             {package} ships when its install is incomplete and the real binary \
             was never put in place."
        ),
        Some(libc::ENOENT) => "The file is gone. It was there when amplihack checked and had \
             disappeared by the time it tried to run it."
            .to_string(),
        Some(libc::EACCES) => "The file is not executable by you.".to_string(),
        _ => "amplihack could not start it.".to_string(),
    };

    format!(
        "Could not launch {path}.\n\n{cause}\n\n{report}",
        path = path.display(),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::test_support::home_env_lock;

    // These tests spawn system binaries (`echo`, `sleep`, `true`, `false`, `sh`).
    // They hold `home_env_lock()` to prevent concurrent fleet/install tests from
    // narrowing PATH (to a temp stub directory) while these tests are running,
    // which would cause the system-binary lookup to fail.

    #[test]
    fn spawn_and_wait_for_exit() {
        let _guard = home_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cmd = Command::new("echo");
        let mut child = ManagedChild::spawn(cmd).unwrap();
        let status = child.wait().unwrap();
        assert!(status.success());
    }

    #[test]
    fn try_wait_returns_none_while_running() {
        let _guard = home_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut cmd = Command::new("sleep");
        cmd.arg("10");
        let mut child = ManagedChild::spawn(cmd).unwrap();

        // Should not have exited yet
        let result = child.try_wait().unwrap();
        assert!(result.is_none());

        // Drop will clean up (SIGTERM → SIGKILL)
    }

    #[test]
    fn drop_terminates_running_process() {
        let _guard = home_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut cmd = Command::new("sleep");
        cmd.arg("60");
        let child = ManagedChild::spawn(cmd).unwrap();
        let pid = child.id();

        // Drop the child — should terminate it
        drop(child);

        // Verify process is gone (on Unix)
        #[cfg(unix)]
        {
            // SAFETY: Sending signal 0 to check if a process exists is a standard
            // POSIX pattern and is safe for any PID value.
            let result = unsafe { libc::kill(i32::try_from(pid).unwrap_or(0), 0) };
            assert_eq!(result, -1, "process should be dead after drop");
        }
    }

    #[test]
    fn managed_child_id() {
        let _guard = home_env_lock()
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let cmd = Command::new("sleep");
        let mut cmd = cmd;
        cmd.arg("0.1");
        let child = ManagedChild::spawn(cmd).unwrap();
        assert!(child.id() > 0);
    }

    #[cfg(unix)]
    #[test]
    fn spawn_keeps_child_in_foreground_process_group() {
        let mut cmd = Command::new("sleep");
        cmd.arg("60");
        let child = ManagedChild::spawn(cmd).unwrap();

        // SAFETY: getpgrp/getpgid only query kernel process-group state.
        let parent_pgid = unsafe { libc::getpgrp() };
        // SAFETY: child.id() is a live child PID here.
        let child_pgid = unsafe { libc::getpgid(i32::try_from(child.id()).unwrap_or(0)) };

        assert_eq!(child_pgid, parent_pgid);
        drop(child);
    }

    // ------------------------------------------------------------------
    // Defect 3 (issue #1266): the spawn failure must name the real cause.
    // ------------------------------------------------------------------

    /// A rejection report as `launch_target::resolve` would produce it after
    /// the stub was the only candidate.
    const STUB_REPORT: &str = "no usable claude binary found\n\n  \
        /home/you/.npm-global/bin/claude   incomplete install — 500-byte \
        placeholder, the native binary was never materialized\n\n  \
        Remedy: npm install -g @anthropic-ai/claude-code";

    fn enriched() -> String {
        // ENOEXEC — the kernel's answer when a 500-byte ASCII file with no
        // shebang is exec'd as a native binary.
        enrich_spawn_error(
            Some(8),
            std::path::Path::new("/home/you/.npm-global/bin/claude"),
            "@anthropic-ai/claude-code",
            STUB_REPORT,
        )
    }

    #[test]
    fn spawn_error_names_the_incomplete_install() {
        let msg = enriched().to_lowercase();
        assert!(
            msg.contains("install")
                && (msg.contains("incomplete")
                    || msg.contains("placeholder")
                    || msg.contains("stub")),
            "must name the real cause, got:\n{msg}"
        );
    }

    #[test]
    fn spawn_error_states_a_remedy() {
        let msg = enriched();
        assert!(
            msg.contains("npm install") && msg.contains("@anthropic-ai/claude-code"),
            "must state a remedy, got:\n{msg}"
        );
    }

    #[test]
    fn spawn_error_names_the_binary_it_tried_to_run() {
        assert!(enriched().contains("/home/you/.npm-global/bin/claude"));
    }

    #[test]
    fn spawn_error_does_not_send_the_user_after_an_arch_problem() {
        let msg = enriched().to_lowercase();
        for forbidden in [
            "exec format error",
            "os error 8",
            "architecture",
            "cpu",
            "platform mismatch",
        ] {
            assert!(
                !msg.contains(forbidden),
                "must not contain {forbidden:?}, got:\n{msg}"
            );
        }
    }

    #[test]
    fn spawn_error_names_the_package_it_was_given_not_claudes() {
        // A copilot user was being told the file was "the placeholder that
        // @anthropic-ai/claude-code ships".
        let msg = enrich_spawn_error(
            Some(8),
            std::path::Path::new("/home/you/.npm-global/bin/copilot"),
            "@github/copilot",
            "no usable copilot binary found",
        );
        assert!(msg.contains("@github/copilot"), "got:\n{msg}");
        assert!(!msg.contains("@anthropic-ai"), "got:\n{msg}");
    }

    #[test]
    fn spawn_error_leaks_no_environment() {
        let msg = enriched();
        for leak in ["PATH=", "HOME=", "NODE_OPTIONS", "AMPLIHACK_"] {
            assert!(!msg.contains(leak), "must not leak {leak:?}, got:\n{msg}");
        }
    }

    #[test]
    fn a_non_enoexec_spawn_failure_still_produces_something_useful() {
        // ENOENT (2): the binary vanished between resolution and exec. Still no
        // architecture talk, still a remedy.
        let msg = enrich_spawn_error(
            Some(2),
            std::path::Path::new("/home/you/.local/bin/claude"),
            "@anthropic-ai/claude-code",
            STUB_REPORT,
        );
        assert!(!msg.is_empty());
        assert!(!msg.to_lowercase().contains("architecture"));
        assert!(msg.contains("/home/you/.local/bin/claude"));
    }
}
