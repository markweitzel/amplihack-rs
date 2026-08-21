//! The single repo-wide answer to "which binary do we launch, is it healthy,
//! and do we need to install anything?".
//!
//! Do not change the signatures below without updating
//! `docs/LAUNCH_TARGET_RESOLUTION.md`, which is the frozen contract.
//!
//! # Why this module exists
//!
//! Before it, three independent resolutions disagreed on a single
//! `amplihack claude` launch: the version check read `/usr/bin/claude`, the
//! install wrote `~/.npm-global/bin/claude`, and the exec ran
//! `~/.local/bin/claude`. Check, install, and exec must all resolve through
//! one function or they will drift apart again.
//!
//! # Security
//!
//! * SEC-3 — probe stdout comes from an arbitrary candidate binary. The
//!   capture is size-capped and ANSI-stripped, on both the version string and
//!   the rejection report (a rejected candidate *path* can itself carry ESC).
//! * SEC-4 — the probe is bounded per-candidate *and* in total, so a hung or
//!   hostile binary early in `$PATH` cannot stall a launch.
//! * SEC-5 — [`is_amplihack_owned_under`] canonicalizes both sides and fails
//!   closed. Ownership drives the write policy, so a false positive means
//!   writing outside amplihack's own prefix.
//! * The health gate is **not** a security boundary. It is a correctness
//!   filter that stops amplihack executing its own broken install. Anyone who
//!   can plant a binary on your `$PATH` can already run code as you.

use crate::binary_finder::{PROBE_CAPTURE_LIMIT, run_capped_output_with_timeout, strip_ansi};
use crate::claude_native::has_native_executable_magic;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::LazyLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// A binary that passed the health gate and may be launched.
///
/// There is no such thing as a `LaunchTarget` with an unknown version. Health
/// is a filter, never an annotation — see [`resolve_from_candidates`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LaunchTarget {
    /// Absolute path to the binary that will be executed.
    pub path: PathBuf,
    /// Parseable semver read from `<path> --version`.
    pub version: String,
    /// Where this candidate was found. Drives the write policy.
    pub source: TargetSource,
}

/// Where a candidate came from, in candidate order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TargetSource {
    /// `AMPLIHACK_CLAUDE_BINARY_PATH` / `CLAUDE_BINARY_PATH`.
    ///
    /// `user_supplied` is false when amplihack set the variable itself
    /// (`commands/rustyclawd.rs` does, in-process). A user-supplied override
    /// that fails the health gate is a hard error; an amplihack-supplied one
    /// warns and falls through, because it is a preference, not an instruction.
    ExplicitOverride {
        /// True when the value came from the caller's environment.
        user_supplied: bool,
    },
    /// Found by walking `$PATH` in order.
    Path,
    /// `~/.npm-global/bin` — the prefix amplihack installs into and owns.
    AmplihackPrefix,
    /// `~/.cargo/bin`, `~/.local/bin`.
    FallbackDir,
}

/// Why a candidate is not a launch target.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Rejection {
    /// No such path, or a dangling symlink.
    Missing,
    /// Resolves to a directory or other non-regular file.
    NotAFile,
    /// No executable bit for this user.
    NotExecutable,
    /// Small, and carrying no native magic number — the placeholder stub.
    StubShape,
    /// `--version` ran but exited non-zero.
    ProbeFailed,
    /// `--version` exceeded the per-candidate budget.
    ProbeTimedOut,
    /// `--version` exited 0 but emitted no parseable semver.
    UnparseableVersion,
}

impl Rejection {
    /// One line naming what is wrong with a candidate, in the user's terms.
    ///
    /// Deliberately never mentions CPU architecture or platform mismatch. The
    /// failure this replaces — `Exec format error (os error 8)` — named nothing
    /// real and sent the user hunting for a hardware problem that did not exist.
    pub fn explain(&self) -> &'static str {
        match self {
            Self::Missing => "not found (no such file, or a broken symlink)",
            Self::NotAFile => "not a regular file",
            Self::NotExecutable => "present but not executable by you",
            Self::StubShape => {
                "incomplete install — this is the small placeholder the npm \
                 package ships, not the native binary it is supposed to be \
                 replaced by"
            }
            Self::ProbeFailed => {
                "`--version` failed — the install is incomplete or the file \
                 cannot be executed"
            }
            Self::ProbeTimedOut => "`--version` did not answer within the probe budget",
            Self::UnparseableVersion => {
                "`--version` reported no usable version, which means the \
                 install cannot be verified"
            }
        }
    }
}

/// The outcome of one resolution pass.
///
/// The rejection list is carried because the error path needs it: a bare
/// `Option<LaunchTarget>` can say "nothing worked" but cannot say what was
/// tried and why each attempt failed.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Resolution {
    /// The first healthy candidate, if any.
    pub target: Option<LaunchTarget>,
    /// Every candidate that was examined and rejected, in candidate order.
    pub rejected: Vec<(PathBuf, Rejection)>,
}

/// What, if anything, amplihack should install.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum InstallDecision {
    /// Launch what is already there. No npm work.
    UseExisting,
    /// Nothing healthy exists anywhere; install into amplihack's own prefix.
    InstallMissing,
    /// The healthy target lives in amplihack's prefix and is stale.
    UpgradeOwned,
}

/// Per-candidate `--version` budget.
///
/// Deliberately larger than `binary_finder`'s 500 ms
/// `VERSION_DETECTION_TIMEOUT`: that constant gates an advisory annotation
/// where a false negative costs nothing, this one gates a launch where a false
/// rejection degrades the user's session.
pub const PER_CANDIDATE_PROBE_TIMEOUT: Duration = Duration::from_secs(3);

/// Total probe budget across every candidate (SEC-4).
pub const TOTAL_PROBE_BUDGET: Duration = Duration::from_secs(10);

/// Hard cap on how many candidates are probed.
pub const MAX_PROBE_CANDIDATES: usize = 8;

/// Files at or below this size that carry no native magic number are the
/// placeholder stub shape.
///
/// 4 KiB is chosen so that a real, non-trivial shell wrapper clears the fast
/// path and is settled by the probe instead. The stub amplihack produces today
/// is 500 bytes.
pub const STUB_MAX_LEN: u64 = 4096;

/// Cheap pre-check: does the head of this file look like the placeholder stub?
///
/// Returns `Some(Rejection::StubShape)` for a **small file that does not begin
/// with a native executable magic number** (`\x7fELF`, a Mach-O magic, or
/// `MZ`). The test is the *absence* of a magic number, not the presence of any
/// particular text: the placeholder shipped by `@anthropic-ai/claude-code` has
/// no shebang — it is 500 bytes beginning
/// `echo "Error: claude native binary not installed." >&2` and `file` reports
/// `ASCII text`. A check written to look for `#!` would miss it.
///
/// This is a fast path, not the authority. Anything it lets through is settled
/// by the `--version` probe.
pub fn classify_head(head: &[u8], len: u64) -> Option<Rejection> {
    if len > STUB_MAX_LEN || has_native_executable_magic(head) {
        return None;
    }
    Some(Rejection::StubShape)
}

/// Extract a parseable semver from `--version` output.
///
/// ANSI escapes are stripped before matching (SEC-3). Returns `None` when the
/// output carries no `\d+\.\d+\.\d+`, which makes the candidate
/// [`Rejection::UnparseableVersion`] — not a target with an unknown version.
pub fn extract_version(output: &str) -> Option<String> {
    static SEMVER: LazyLock<regex::Regex> =
        LazyLock::new(|| regex::Regex::new(r"\d+\.\d+\.\d+").expect("static semver regex"));
    // SEC-3: strip BEFORE matching, so an ESC sequence can neither hide inside
    // the captured version nor survive into the log line or the user's TTY.
    let cleaned = strip_ansi(output);
    SEMVER.find(&cleaned).map(|m| m.as_str().to_string())
}

/// Is `path` inside `prefix`, and therefore amplihack's to overwrite?
///
/// SEC-5: canonicalizes both sides and **fails closed**. If either side cannot
/// be canonicalized the answer is `false` — amplihack does not write to
/// anything it cannot prove it owns.
pub fn is_amplihack_owned_under(path: &Path, prefix: &Path) -> bool {
    let (Ok(path), Ok(prefix)) = (path.canonicalize(), prefix.canonicalize()) else {
        // SEC-5: unresolvable on either side means amplihack cannot prove it
        // owns the target, so it does not write there. Fail closed.
        return false;
    };
    path.starts_with(&prefix)
}

/// The entire fix for the reinstall-on-every-launch defect, as a pure function.
///
/// Two rules are load-bearing:
///
/// * **amplihack never upgrades a binary it does not own.** If the binary that
///   will actually be executed lives outside `~/.npm-global`, installing into
///   that prefix would not change what gets launched — so the "upgrade" is
///   hundreds of megabytes with no effect, and the next launch decides
///   identically, forever. That loop is the defect.
/// * **A failed registry query never triggers an install.** `latest == None`
///   means "unknown", not "stale". A network blip must not cost 339 MB.
pub fn decide_install(target: Option<&LaunchTarget>, latest: Option<&str>) -> InstallDecision {
    let Some(target) = target else {
        return InstallDecision::InstallMissing;
    };
    if target.source != TargetSource::AmplihackPrefix {
        // The binary that will actually be executed lives somewhere amplihack
        // does not write. Installing into amplihack's prefix cannot change what
        // gets launched, so the "upgrade" is hundreds of megabytes with no
        // effect and the next launch decides identically. Forever.
        return InstallDecision::UseExisting;
    }
    match latest {
        // A failed registry query means "unknown", never "stale".
        None => InstallDecision::UseExisting,
        Some(latest) if latest == target.version => InstallDecision::UseExisting,
        Some(_) => InstallDecision::UpgradeOwned,
    }
}

/// The I/O shell: probe `candidates` in order and return the first healthy one.
///
/// Split from [`candidate_paths`] so the health gate is testable against a
/// temp-dir fixture without mutating process environment (which is `unsafe`
/// under edition 2024).
///
/// Probing stops at the first healthy candidate and is bounded by
/// [`MAX_PROBE_CANDIDATES`], [`PER_CANDIDATE_PROBE_TIMEOUT`], and
/// [`TOTAL_PROBE_BUDGET`].
pub fn resolve_from_candidates(tool: &str, candidates: &[(PathBuf, TargetSource)]) -> Resolution {
    let mut resolution = Resolution::default();
    let started = Instant::now();
    let mut probes = 0usize;

    for (path, source) in candidates {
        // The cheap checks are free, so they do not consume the probe budget:
        // a $PATH with thirty entries and one binary must not exhaust it before
        // reaching the binary.
        let rejection = match cheap_reject(path) {
            Some(rejection) => Some(rejection),
            None => {
                if probes >= MAX_PROBE_CANDIDATES {
                    tracing::warn!(
                        tool,
                        max = MAX_PROBE_CANDIDATES,
                        "candidate probe cap reached; stopping resolution"
                    );
                    break;
                }
                let Some(budget) = TOTAL_PROBE_BUDGET.checked_sub(started.elapsed()) else {
                    tracing::warn!(
                        tool,
                        budget = ?TOTAL_PROBE_BUDGET,
                        "total probe budget exhausted; stopping resolution"
                    );
                    break;
                };
                probes += 1;
                match probe_version(path, PER_CANDIDATE_PROBE_TIMEOUT.min(budget)) {
                    Ok(version) => {
                        tracing::debug!(
                            tool,
                            path = %path.display(),
                            version,
                            ?source,
                            "resolved launch target"
                        );
                        resolution.target = Some(LaunchTarget {
                            path: path.clone(),
                            version,
                            source: *source,
                        });
                        return resolution;
                    }
                    Err(rejection) => Some(rejection),
                }
            }
        };

        let Some(rejection) = rejection else {
            continue;
        };
        tracing::debug!(
            tool,
            path = %path.display(),
            ?rejection,
            ?source,
            "candidate rejected"
        );
        resolution.rejected.push((path.clone(), rejection));

        // A user who names a specific binary and gets a broken one is told so.
        // Silently launching a different binary than the one they asked for is
        // the behaviour this whole module exists to remove.
        if *source
            == (TargetSource::ExplicitOverride {
                user_supplied: true,
            })
        {
            tracing::error!(
                tool,
                path = %path.display(),
                ?rejection,
                "explicit binary override failed the health gate"
            );
            return resolution;
        }
        if matches!(source, TargetSource::ExplicitOverride { .. }) {
            tracing::warn!(
                tool,
                path = %path.display(),
                ?rejection,
                "amplihack-set binary preference failed the health gate; falling through"
            );
        }
    }

    resolution
}

/// Filesystem-only checks. No subprocess, so these never consume the probe
/// budget.
fn cheap_reject(path: &Path) -> Option<Rejection> {
    // `metadata` FOLLOWS symlinks, and it must: every npm-installed claude on
    // every host is a symlink into
    // lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe. Using
    // `symlink_metadata` here would reject them all, amplihack's own install
    // included. A dangling link correctly surfaces as `Missing`.
    let Ok(metadata) = std::fs::metadata(path) else {
        return Some(Rejection::Missing);
    };
    if !metadata.is_file() {
        return Some(Rejection::NotAFile);
    }
    if !is_executable(&metadata) {
        return Some(Rejection::NotExecutable);
    }
    let len = metadata.len();
    let mut head = [0u8; 8];
    let read = std::fs::File::open(path)
        .and_then(|mut f| f.read(&mut head))
        .unwrap_or(0);
    classify_head(&head[..read], len)
}

#[cfg(unix)]
fn is_executable(metadata: &std::fs::Metadata) -> bool {
    use std::os::unix::fs::MetadataExt;
    metadata.mode() & 0o111 != 0
}

#[cfg(not(unix))]
fn is_executable(_metadata: &std::fs::Metadata) -> bool {
    // Windows has no executable bit; extension-based execution is the norm and
    // the `--version` probe is the authority there.
    true
}

/// Run `<path> --version` and require a parseable semver from a clean exit.
fn probe_version(path: &Path, timeout: Duration) -> Result<String, Rejection> {
    let mut cmd = Command::new(path);
    cmd.arg("--version");
    match run_capped_output_with_timeout(cmd, timeout, PROBE_CAPTURE_LIMIT) {
        Ok(Some(output)) if output.status.success() => {
            // SEC-3: stdout here is whatever an arbitrary binary chose to
            // print. It is capped above and ANSI-stripped inside
            // `extract_version`, and nothing but the matched semver survives.
            extract_version(&String::from_utf8_lossy(&output.stdout))
                .ok_or(Rejection::UnparseableVersion)
        }
        Ok(Some(_)) => Err(Rejection::ProbeFailed),
        Ok(None) => Err(Rejection::ProbeTimedOut),
        // A spawn failure is the ENOEXEC case among others: the file is there
        // and executable but the kernel will not run it. That is a failed
        // install, not a launch target.
        Err(_) => Err(Rejection::ProbeFailed),
    }
}

/// Set when amplihack itself wrote `AMPLIHACK_CLAUDE_BINARY_PATH`, so
/// [`candidate_paths`] can tell its own preference from a user's instruction.
///
/// An in-process flag rather than a second environment variable on purpose: an
/// env marker would be inherited by nested `amplihack` invocations and would
/// silently downgrade a genuine user override into a preference.
static OVERRIDE_IS_AMPLIHACK_SUPPLIED: AtomicBool = AtomicBool::new(false);

/// Record that the binary-path override in the environment was set by
/// amplihack, not by the user. Called by `configure_preferred_rustyclawd_binary`.
pub fn mark_override_amplihack_supplied() {
    OVERRIDE_IS_AMPLIHACK_SUPPLIED.store(true, Ordering::Relaxed);
}

/// Build the candidate list for `tool` from the environment, in this order:
///
/// 1. `AMPLIHACK_{TOOL}_BINARY_PATH`, then `{TOOL}_BINARY_PATH`
/// 2. each `$PATH` entry, in `$PATH` order
/// 3. `~/.npm-global/bin` — amplihack's own prefix
/// 4. the remaining fallback dirs: `~/.cargo/bin`, `~/.local/bin`
pub fn candidate_paths(tool: &str) -> Vec<(PathBuf, TargetSource)> {
    let mut candidates: Vec<(PathBuf, TargetSource)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    let mut push = |candidates: &mut Vec<(PathBuf, TargetSource)>, path: PathBuf, source| {
        if seen.insert(path.clone()) {
            candidates.push((path, source));
        }
    };

    let tool_upper = tool.to_uppercase();
    if let Some(value) = std::env::var_os(format!("AMPLIHACK_{tool_upper}_BINARY_PATH")) {
        let user_supplied = !OVERRIDE_IS_AMPLIHACK_SUPPLIED.load(Ordering::Relaxed);
        push(
            &mut candidates,
            PathBuf::from(value),
            TargetSource::ExplicitOverride { user_supplied },
        );
    }
    if let Some(value) = std::env::var_os(format!("{tool_upper}_BINARY_PATH")) {
        push(
            &mut candidates,
            PathBuf::from(value),
            TargetSource::ExplicitOverride {
                user_supplied: true,
            },
        );
    }

    // Directory list, tagged by who owns it. Ownership is decided by WHERE a
    // directory is, never by how it was discovered: on the repo owner's WSL
    // machine `~/.npm-global/bin` is the FIRST $PATH entry, and tagging it
    // `Path` there would tell `decide_install` that amplihack does not own its
    // own install and must never upgrade it.
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from);
    let npm_prefix_bin = home.as_ref().map(|h| h.join(".npm-global").join("bin"));
    let fallback_dirs: Vec<PathBuf> = home
        .as_ref()
        .map(|h| vec![h.join(".cargo").join("bin"), h.join(".local").join("bin")])
        .unwrap_or_default();
    let source_for = |dir: &Path| {
        if npm_prefix_bin.as_deref() == Some(dir) {
            TargetSource::AmplihackPrefix
        } else if fallback_dirs.iter().any(|d| d == dir) {
            TargetSource::FallbackDir
        } else {
            TargetSource::Path
        }
    };

    let mut dirs: Vec<PathBuf> = Vec::new();
    if let Some(path_var) = std::env::var_os("PATH") {
        dirs.extend(std::env::split_paths(&path_var));
    }
    // Known install targets, appended in case the user's shell PATH predates
    // amplihack's own install (persistent tmux/ssh sessions, minimal Docker
    // shells). Already-present entries keep their $PATH position.
    if let Some(npm_bin) = npm_prefix_bin.clone() {
        dirs.push(npm_bin);
    }
    dirs.extend(fallback_dirs.iter().cloned());

    // Candidate-major, matching `binary_finder::binary_candidates`: a
    // `rustyclawd` anywhere on $PATH outranks a `claude`, which is the existing
    // and intended precedence for the RustyClawd front end.
    let names = crate::binary_finder::binary_candidates(tool);
    let mut dedup_dirs: Vec<PathBuf> = Vec::new();
    for dir in dirs {
        if !dedup_dirs.contains(&dir) {
            dedup_dirs.push(dir);
        }
    }
    for name in &names {
        for dir in &dedup_dirs {
            let path = dir.join(name);
            let source = source_for(dir);
            push(&mut candidates, path, source);
        }
    }

    candidates
}

/// Resolve the launch target for `tool`.
///
/// This is the only function in the repo permitted to answer "which binary" for
/// a launch, an install decision, or a version check. See
/// `docs/LAUNCH_TARGET_RESOLUTION.md`.
pub fn resolve(tool: &str) -> Resolution {
    resolve_from_candidates(tool, &candidate_paths(tool))
}

impl Resolution {
    /// Human-readable account of every candidate that was rejected and why,
    /// plus the remedy.
    ///
    /// Carries paths, rejection reasons, and the remedy — never the
    /// environment, never the full argv. ANSI escapes in candidate paths are
    /// stripped (SEC-3).
    pub fn rejection_report(&self) -> String {
        let mut out = String::from(
            "No usable claude binary was found. Every candidate below was \
             examined and rejected:\n",
        );
        for (path, rejection) in &self.rejected {
            // SEC-3: a planted filename can carry ESC. Strip before it reaches
            // the terminal.
            out.push_str(&format!(
                "\n  {}\n      {}\n",
                strip_ansi(&path.display().to_string()),
                rejection.explain()
            ));
        }
        out.push_str(
            "\nRemedy: install a complete copy of the Claude Code CLI, then run \
             amplihack again:\n  \
             npm install -g @anthropic-ai/claude-code\n\
             The npm package materializes its ~339 MB native binary in its \
             postinstall step; an install that skipped that step leaves the \
             placeholder above behind.\n",
        );
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // classify_head — the cheap stub pre-check
    // ------------------------------------------------------------------

    /// The exact bytes of the stub amplihack produces today, verified on the
    /// dev VM 2026-08-21: 500 bytes, ASCII, **no shebang**.
    fn real_stub_bytes() -> Vec<u8> {
        let mut v = b"echo \"Error: claude native binary not installed.\" >&2\nexit 1\n".to_vec();
        v.resize(500, b' ');
        v
    }

    #[test]
    fn classify_head_rejects_the_real_500_byte_stub() {
        let stub = real_stub_bytes();
        assert_eq!(
            classify_head(&stub, stub.len() as u64),
            Some(Rejection::StubShape),
            "the 500-byte ASCII placeholder is the exact shape this gate exists to catch"
        );
    }

    #[test]
    fn classify_head_does_not_look_for_a_shebang() {
        // The real stub has no `#!`. A gate written as "starts with #!" would
        // miss it entirely, so the test must be the ABSENCE of native magic.
        let stub = real_stub_bytes();
        assert!(
            !stub.starts_with(b"#!"),
            "fixture invariant: the real stub carries no shebang"
        );
        assert_eq!(
            classify_head(&stub, stub.len() as u64),
            Some(Rejection::StubShape)
        );
    }

    #[test]
    fn classify_head_accepts_elf() {
        assert_eq!(classify_head(b"\x7fELF\x02\x01\x01\x00", 338_860_336), None);
    }

    #[test]
    fn classify_head_accepts_mach_o() {
        // 64-bit Mach-O, both endiannesses.
        assert_eq!(classify_head(&[0xcf, 0xfa, 0xed, 0xfe], 90_000_000), None);
        assert_eq!(classify_head(&[0xfe, 0xed, 0xfa, 0xcf], 90_000_000), None);
    }

    #[test]
    fn classify_head_accepts_pe() {
        assert_eq!(classify_head(b"MZ\x90\x00", 120_000_000), None);
    }

    #[test]
    fn classify_head_accepts_a_large_shell_wrapper() {
        // A real 8 KiB shell wrapper is over the stub threshold, so the fast
        // path lets it through and the --version probe settles it. The gate
        // must not guess about anything it is not sure of.
        let wrapper = b"#!/bin/sh\nexec node /opt/claude/cli.js \"$@\"\n";
        assert_eq!(classify_head(wrapper, 8192), None);
    }

    #[test]
    fn classify_head_accepts_an_empty_head_of_a_large_file() {
        // Unreadable head but a plausible size: not our call to make. Let the
        // probe decide rather than rejecting a working binary.
        assert_eq!(classify_head(&[], 338_860_336), None);
    }

    #[test]
    fn classify_head_rejects_a_zero_byte_file() {
        assert_eq!(classify_head(&[], 0), Some(Rejection::StubShape));
    }

    // ------------------------------------------------------------------
    // extract_version
    // ------------------------------------------------------------------

    #[test]
    fn extract_version_parses_the_real_claude_output() {
        assert_eq!(
            extract_version("2.1.238 (Claude Code)\n").as_deref(),
            Some("2.1.238")
        );
    }

    #[test]
    fn extract_version_strips_ansi_before_matching() {
        // SEC-3: probe stdout is attacker-controlled. An ESC sequence must not
        // reach the version string or the user's TTY.
        let raw = "\x1b[32m2.1.238\x1b[0m (Claude Code)";
        let parsed = extract_version(raw).expect("version behind ANSI must still parse");
        assert_eq!(parsed, "2.1.238");
        assert!(!parsed.contains('\x1b'), "no ESC may survive: {parsed:?}");
    }

    #[test]
    fn extract_version_rejects_the_stubs_error_text() {
        // The stub's own message, should it ever exit 0.
        assert_eq!(
            extract_version("Error: claude native binary not installed."),
            None
        );
    }

    #[test]
    fn extract_version_rejects_unknown_and_empty() {
        assert_eq!(extract_version("unknown"), None);
        assert_eq!(extract_version(""), None);
        assert_eq!(extract_version("   \n\n"), None);
    }

    #[test]
    fn extract_version_rejects_a_two_component_version() {
        // "2.1" is not a semver; an unparseable version is a rejection, never
        // a target annotated `version: "unknown"`.
        assert_eq!(extract_version("claude 2.1"), None);
    }

    // ------------------------------------------------------------------
    // is_amplihack_owned_under — SEC-5, fails closed
    // ------------------------------------------------------------------

    #[test]
    fn owned_under_accepts_a_path_inside_the_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("npm-global");
        let bin = prefix.join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        let target = bin.join("claude");
        std::fs::write(&target, b"x").unwrap();
        assert!(is_amplihack_owned_under(&target, &prefix));
    }

    #[test]
    fn owned_under_rejects_a_sibling_prefix() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("npm-global");
        let other = dir.path().join("npm-global-evil");
        std::fs::create_dir_all(&prefix).unwrap();
        std::fs::create_dir_all(&other).unwrap();
        let target = other.join("claude");
        std::fs::write(&target, b"x").unwrap();
        assert!(
            !is_amplihack_owned_under(&target, &prefix),
            "a shared string prefix is not containment"
        );
    }

    #[test]
    fn owned_under_rejects_a_traversal_escape() {
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("npm-global");
        std::fs::create_dir_all(prefix.join("bin")).unwrap();
        let outside = dir.path().join("claude");
        std::fs::write(&outside, b"x").unwrap();
        let sneaky = prefix.join("bin").join("..").join("..").join("claude");
        assert!(!is_amplihack_owned_under(&sneaky, &prefix));
    }

    #[test]
    fn owned_under_fails_closed_when_the_path_does_not_exist() {
        // SEC-5: unresolvable => not owned => amplihack writes nothing.
        let dir = tempfile::tempdir().unwrap();
        let prefix = dir.path().join("npm-global");
        std::fs::create_dir_all(&prefix).unwrap();
        assert!(!is_amplihack_owned_under(
            &prefix.join("bin/claude"),
            &prefix
        ));
    }

    #[test]
    fn owned_under_fails_closed_when_the_prefix_does_not_exist() {
        let dir = tempfile::tempdir().unwrap();
        let target = dir.path().join("claude");
        std::fs::write(&target, b"x").unwrap();
        assert!(!is_amplihack_owned_under(&target, &dir.path().join("nope")));
    }

    // ------------------------------------------------------------------
    // decide_install — the whole of Defect 2, as a table
    // ------------------------------------------------------------------

    fn target(source: TargetSource, version: &str) -> LaunchTarget {
        LaunchTarget {
            path: PathBuf::from("/anywhere/claude"),
            version: version.to_string(),
            source,
        }
    }

    #[test]
    fn decide_install_installs_when_nothing_healthy_exists() {
        assert_eq!(
            decide_install(None, Some("2.1.238")),
            InstallDecision::InstallMissing
        );
        assert_eq!(decide_install(None, None), InstallDecision::InstallMissing);
    }

    #[test]
    fn decide_install_never_upgrades_a_binary_amplihack_does_not_own() {
        // THE reinstall loop, verified on dev: /usr/bin/claude @ 2.1.237 is
        // healthy and on PATH, the registry says 2.1.238. Installing into
        // ~/.npm-global cannot change what gets launched, so "upgrading" is
        // 339 MB of download and the next launch decides identically. Forever.
        for source in [
            TargetSource::Path,
            TargetSource::FallbackDir,
            TargetSource::ExplicitOverride {
                user_supplied: true,
            },
            TargetSource::ExplicitOverride {
                user_supplied: false,
            },
        ] {
            assert_eq!(
                decide_install(Some(&target(source, "2.1.237")), Some("2.1.238")),
                InstallDecision::UseExisting,
                "must not upgrade a non-owned target ({source:?})"
            );
        }
    }

    #[test]
    fn decide_install_upgrades_a_stale_binary_in_amplihacks_own_prefix() {
        assert_eq!(
            decide_install(
                Some(&target(TargetSource::AmplihackPrefix, "2.1.237")),
                Some("2.1.238")
            ),
            InstallDecision::UpgradeOwned
        );
    }

    #[test]
    fn decide_install_does_nothing_when_the_owned_binary_is_current() {
        // A7's second run: zero npm work, no 339 MB download.
        assert_eq!(
            decide_install(
                Some(&target(TargetSource::AmplihackPrefix, "2.1.238")),
                Some("2.1.238")
            ),
            InstallDecision::UseExisting
        );
    }

    #[test]
    fn decide_install_never_installs_when_the_registry_query_failed() {
        // latest == None means "unknown", not "stale". A network blip must
        // never cost the user a reinstall.
        assert_eq!(
            decide_install(
                Some(&target(TargetSource::AmplihackPrefix, "2.1.237")),
                None
            ),
            InstallDecision::UseExisting
        );
    }

    // ------------------------------------------------------------------
    // rejection_report — Defect 3's message contract (A-AMB-11)
    // ------------------------------------------------------------------

    fn stub_and_timeout_resolution() -> Resolution {
        Resolution {
            target: None,
            rejected: vec![
                (
                    PathBuf::from("/home/you/.npm-global/bin/claude"),
                    Rejection::StubShape,
                ),
                (
                    PathBuf::from("/home/you/.local/bin/claude"),
                    Rejection::ProbeTimedOut,
                ),
            ],
        }
    }

    #[test]
    fn rejection_report_names_the_real_cause() {
        let report = stub_and_timeout_resolution()
            .rejection_report()
            .to_lowercase();
        assert!(
            report.contains("install")
                && (report.contains("incomplete")
                    || report.contains("stub")
                    || report.contains("placeholder")),
            "must name the incomplete-install cause, got:\n{report}"
        );
    }

    #[test]
    fn rejection_report_states_a_remedy() {
        let report = stub_and_timeout_resolution().rejection_report();
        assert!(
            report.contains("npm install") && report.contains("@anthropic-ai/claude-code"),
            "must state a copy-pasteable remedy, got:\n{report}"
        );
    }

    #[test]
    fn rejection_report_lists_every_rejected_candidate() {
        let report = stub_and_timeout_resolution().rejection_report();
        assert!(report.contains("/home/you/.npm-global/bin/claude"));
        assert!(report.contains("/home/you/.local/bin/claude"));
    }

    #[test]
    fn rejection_report_does_not_send_the_user_hunting_for_an_arch_problem() {
        // The old message was `Exec format error (os error 8)`, which named
        // nothing real and pointed at a CPU-architecture problem that did not
        // exist.
        let report = stub_and_timeout_resolution()
            .rejection_report()
            .to_lowercase();
        for forbidden in [
            "exec format error",
            "os error 8",
            "architecture",
            "arch mismatch",
            "platform mismatch",
        ] {
            assert!(
                !report.contains(forbidden),
                "report must not contain {forbidden:?}, got:\n{report}"
            );
        }
    }

    #[test]
    fn rejection_report_strips_ansi_from_candidate_paths() {
        // SEC-3: a planted ~/.local/bin/<ESC>… filename must not be rendered
        // into the user's terminal.
        let resolution = Resolution {
            target: None,
            rejected: vec![(
                PathBuf::from("/tmp/\x1b[2J\x1b[Hclaude"),
                Rejection::StubShape,
            )],
        };
        let report = resolution.rejection_report();
        assert!(
            !report.contains('\x1b'),
            "no ESC may reach the TTY, got: {report:?}"
        );
    }

    #[test]
    fn rejection_report_carries_no_environment() {
        // Error text carries paths, reasons, and the remedy. Nothing else.
        let report = stub_and_timeout_resolution().rejection_report();
        for leak in ["PATH=", "HOME=", "AMPLIHACK_", "NODE_OPTIONS"] {
            assert!(
                !report.contains(leak),
                "report must not leak {leak:?}, got:\n{report}"
            );
        }
    }
}
