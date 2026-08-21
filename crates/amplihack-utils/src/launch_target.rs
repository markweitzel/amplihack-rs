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
//! # What this module is not
//!
//! It is tool-generic, and the body has to stay that way. Every function here
//! takes a `tool`, and a fact that is only true of `@anthropic-ai/claude-code`
//! belongs in [`crate::claude_native`], not in a check that runs for copilot
//! and codex too. The one time that boundary was crossed — a "small file with
//! no native magic is a broken install" gate — it rejected `@github/copilot`'s
//! legitimate 1185-byte `#!/usr/bin/env node` loader and broke the launch.
//!
//! # Security
//!
//! * SEC-3 — probe stdout comes from an arbitrary candidate binary. The
//!   capture is size-capped and passed through
//!   [`crate::binary_finder::strip_ansi`], which removes CSI and OSC/DCS/APC
//!   sequences, two-byte escapes, and turns every remaining C0 control into a
//!   space. Applied to both the version string and the rejection report (a
//!   rejected candidate *path* can itself carry ESC, and a newline in a path
//!   would otherwise forge extra rows in the report).
//! * SEC-4 — the probe is bounded per-candidate *and* in total. The bound
//!   covers the whole subprocess, output drain included: a child that exits
//!   while a grandchild holds its stdout pipe open is not allowed to stall the
//!   launch, it costs a truncated capture instead. So a hung or hostile binary
//!   early in `$PATH` cannot stall a launch.
//! * SEC-5 — ownership drives the write policy, and it is decided by
//!   [`TargetSource`] alone: only a candidate found in amplihack's own prefix
//!   directory is ever written to. A directory that is spelled differently
//!   from that prefix is tagged [`TargetSource::Path`] and therefore left
//!   alone, so the failure mode is "amplihack declines to upgrade", never
//!   "amplihack writes outside its prefix".
//! * The health gate is **not** a security boundary. It is a correctness
//!   filter that stops amplihack executing its own broken install. Anyone who
//!   can plant a binary on your `$PATH` can already run code as you.

use crate::binary_finder::{PROBE_CAPTURE_LIMIT, run_capped_output_with_timeout, strip_ansi};
use crate::claude_native::has_placeholder_shape;
use std::collections::HashMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{LazyLock, Mutex};
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
    /// `--version` failed *and* the file has the placeholder's shape.
    ///
    /// A refinement of [`Self::ProbeFailed`], never an independent verdict —
    /// see [`label_failed_probe`].
    PlaceholderStub,
    /// `--version` failed and the file could not be read to say why.
    Unreadable,
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
            Self::PlaceholderStub => {
                "incomplete install — `--version` failed and the file is the \
                 small placeholder the npm package ships, not the native \
                 binary it is supposed to be replaced by"
            }
            Self::Unreadable => "`--version` failed and the file could not be read to diagnose it",
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
    /// Nothing healthy resolved, but the evidence is inconclusive: at least one
    /// candidate timed out rather than answering.
    ///
    /// Same rule `decide_install` already applies to a failed registry query,
    /// on the other axis. A 3 s `--version` timeout on a loaded box is the same
    /// class of transient as a network blip, and neither is worth ~339 MB. The
    /// caller reports it and stops instead of installing over a binary that may
    /// well be fine.
    Abstain,
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
/// * **Inconclusive evidence never triggers an install either.** The same rule,
///   on the resolution axis: if nothing healthy resolved *because a candidate
///   timed out*, amplihack does not know whether a working binary is there. It
///   answers [`InstallDecision::Abstain`]. This is why the whole
///   [`Resolution`] is the input and not just its target — the rejection list
///   is the difference between "nothing is installed" and "we could not tell".
pub fn decide_install(resolution: &Resolution, latest: Option<&str>) -> InstallDecision {
    let Some(target) = resolution.target.as_ref() else {
        // A timeout is not evidence of absence. Anything else in the list is:
        // Missing, NotExecutable, PlaceholderStub and a non-zero probe all say
        // "there is no working binary here", which is what an install fixes.
        if resolution
            .rejected
            .iter()
            .any(|(_, rejection)| *rejection == Rejection::ProbeTimedOut)
        {
            return InstallDecision::Abstain;
        }
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

/// Can this path be executed at all? Filesystem facts only.
///
/// No subprocess, so these never consume the probe budget — and **no judgement
/// about the file's contents**. Every check here is true for any tool on any
/// platform: it is there, it is a file, you may run it.
///
/// It used to also reject a small file with no native executable magic, as a
/// fast path that saved one `execve` on a claude install that was already
/// broken. That is a claude-shaped fact, and running it as a gate for every
/// tool broke `amplihack copilot`, whose `@github/copilot` loader is a
/// legitimate 1185-byte `#!/usr/bin/env node` shim. The knowledge was not lost
/// — [`label_failed_probe`] still uses it to *name* a failure — but it can no
/// longer cause one. Do not move it back.
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
    None
}

/// Put the right words on a candidate whose `--version` probe has already
/// failed.
///
/// This can only ever *rename* an existing rejection, so — unlike the
/// pre-probe gate it replaces — it cannot reject a candidate the probe would
/// have accepted, for any tool, present or future. That property is the whole
/// design: the boundary violation is gone by construction rather than by an
/// `if tool == "claude"` that the next tool re-opens.
///
/// The good diagnosis survives: an incomplete `@anthropic-ai/claude-code`
/// install is still reported as an incomplete install rather than as a generic
/// "`--version` failed", which is the message this whole module exists to
/// improve.
fn label_failed_probe(path: &Path) -> Rejection {
    let Ok(metadata) = std::fs::metadata(path) else {
        // It answered `cheap_reject` a moment ago, so this is a genuine I/O
        // failure, not absence. Say that, rather than asserting something
        // about contents nobody read.
        return Rejection::Unreadable;
    };
    let mut head = [0u8; 8];
    let Ok(read) = std::fs::File::open(path).and_then(|mut f| f.read(&mut head)) else {
        return Rejection::Unreadable;
    };
    if has_placeholder_shape(&head[..read], metadata.len()) {
        return Rejection::PlaceholderStub;
    }
    Rejection::ProbeFailed
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
        Ok(Some(_)) => Err(label_failed_probe(path)),
        Ok(None) => Err(Rejection::ProbeTimedOut),
        // A spawn failure is the ENOEXEC case among others: the file is there
        // and executable but the kernel will not run it. That is a failed
        // install, not a launch target.
        Err(_) => Err(label_failed_probe(path)),
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
    //
    // `dirs` may repeat an entry (a $PATH that already names the npm prefix,
    // appended again below). No separate directory de-duplication pass is
    // needed: `push` de-duplicates on the joined path and keeps the first
    // occurrence, so a repeated directory contributes nothing the first one
    // did not.
    for name in &crate::binary_finder::binary_candidates(tool) {
        for dir in &dirs {
            push(&mut candidates, dir.join(name), source_for(dir));
        }
    }

    candidates
}

/// Per-tool memo of the last resolution, with the candidate list it was
/// computed from.
///
/// Keyed by tool and bounded by the number of tools, so it cannot grow.
static RESOLUTION_MEMO: LazyLock<Mutex<HashMap<String, (Candidates, Resolution)>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));

/// A candidate list, as [`candidate_paths`] produces it.
type Candidates = Vec<(PathBuf, TargetSource)>;

/// Resolve the launch target for `tool`.
///
/// This is the only function in the repo permitted to answer "which binary" for
/// a launch, an install decision, or a version check. See
/// `docs/LAUNCH_TARGET_RESOLUTION.md`.
///
/// # Memoized
///
/// One launch asks this question at least twice — the update notice, then the
/// install decision — and each answer costs a `--version` subprocess against a
/// ~339 MB binary (measured: 151 ms per resolution on the dev VM, of which
/// 0.15 ms is building the candidate list). The repeats are pure waste: the
/// question is "which binary will we launch", and it has one answer per
/// process.
///
/// The memo is keyed by tool and validated against the **candidate list**, not
/// just the name. Every input `resolve_from_candidates` reads is in that list,
/// so any environment change that could change the answer — `PATH`, `HOME`, an
/// override variable, [`mark_override_amplihack_supplied`] — produces a
/// different list and misses the memo rather than returning a stale answer.
///
/// What the memo cannot see is the filesystem changing underneath it, which is
/// exactly what an install does. That path calls [`resolve_uncached`].
pub fn resolve(tool: &str) -> Resolution {
    let candidates = candidate_paths(tool);
    // The probe runs under the lock, and the lock is ONE mutex for ALL tools,
    // not one per tool. So a slow `claude` resolution also delays a concurrent
    // `copilot` one. Accepted, because the wait it can impose is bounded:
    // `resolve_from_candidates` returns within `TOTAL_PROBE_BUDGET`, and that
    // bound covers the output drain as well as the wait (SEC-4). For a second
    // thread asking the SAME question — the case that actually happens, since a
    // launch asks about one tool — waiting for the answer beats racing for it.
    // Split the map per tool if a real workload ever resolves two tools at once.
    let mut memo = RESOLUTION_MEMO
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Some((memoized_candidates, resolution)) = memo.get(tool)
        && *memoized_candidates == candidates
    {
        return resolution.clone();
    }
    let resolution = resolve_from_candidates(tool, &candidates);
    memo.insert(tool.to_string(), (candidates, resolution.clone()));
    resolution
}

/// Resolve `tool` ignoring the memo, and refresh it with the answer.
///
/// For callers that just changed the filesystem — i.e. installed something.
/// Nothing else should need it: the memo already misses on any environment
/// change that could matter.
pub fn resolve_uncached(tool: &str) -> Resolution {
    let candidates = candidate_paths(tool);
    let resolution = resolve_from_candidates(tool, &candidates);
    RESOLUTION_MEMO
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
        .insert(tool.to_string(), (candidates, resolution.clone()));
    resolution
}

impl Resolution {
    /// Human-readable account of what happened, and the remedy.
    ///
    /// `tool` and `package` are parameters because this is the error path for
    /// **every** tool. It used to hardcode claude's name and claude's npm
    /// package, so a copilot user was told "No usable claude binary was found"
    /// and instructed to install `@anthropic-ai/claude-code` — a regression in
    /// exactly the surface this function exists to improve.
    ///
    /// Two headlines, because there are two failures and they need different
    /// words. When nothing resolved, the list below *is* the story. When
    /// something did resolve and the caller is reporting a failure anyway — the
    /// spawn path — the binary that failed is the one that is missing from the
    /// list, and "no usable binary was found" over a list that does not contain
    /// it (and is usually empty) is simply false.
    ///
    /// Carries paths, rejection reasons, and the remedy — never the
    /// environment, never the full argv. ANSI escapes and control characters in
    /// candidate paths are stripped (SEC-3).
    pub fn rejection_report(&self, tool: &str, package: &str) -> String {
        let mut out = match self.target.as_ref() {
            Some(target) => format!(
                "amplihack selected {path} (version {version}) for {tool}, and it \
                 could not be run.\n",
                path = strip_ansi(&target.path.display().to_string()),
                version = strip_ansi(&target.version),
            ),
            None => format!(
                "No usable {tool} binary was found. Every candidate below was \
                 examined and rejected:\n"
            ),
        };
        for (path, rejection) in &self.rejected {
            // SEC-3: a planted filename can carry ESC, and a newline in it
            // would forge extra rows in this very report. Strip before it
            // reaches the terminal.
            out.push_str(&format!(
                "\n  {}\n      {}\n",
                strip_ansi(&path.display().to_string()),
                rejection.explain()
            ));
        }
        out.push_str(&format!(
            "\nRemedy: install a complete copy of {tool}, then run amplihack \
             again:\n  \
             npm install -g {package}\n\
             A package whose postinstall step was skipped leaves a small \
             placeholder behind instead of the binary it is supposed to \
             install.\n"
        ));
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ------------------------------------------------------------------
    // cheap_reject / label_failed_probe — the diagnosis is not a gate
    // ------------------------------------------------------------------

    /// The exact bytes of the placeholder amplihack has seen in the wild:
    /// 500 bytes, ASCII, **no shebang**. Verified on the dev VM 2026-08-21.
    fn real_stub_bytes() -> Vec<u8> {
        let mut v = b"echo \"Error: claude native binary not installed.\" >&2\nexit 1\n".to_vec();
        v.resize(500, b' ');
        v
    }

    #[cfg(unix)]
    fn write_executable(dir: &Path, name: &str, body: &[u8]) -> PathBuf {
        use std::os::unix::fs::PermissionsExt;
        let path = dir.join(name);
        std::fs::write(&path, body).unwrap();
        let mut perms = std::fs::metadata(&path).unwrap().permissions();
        perms.set_mode(0o755);
        std::fs::set_permissions(&path, perms).unwrap();
        path
    }

    #[cfg(unix)]
    #[test]
    fn cheap_reject_passes_a_small_shim_it_cannot_judge() {
        // THE regression. `~/.npm-global/bin/copilot` is a 1185-byte
        // `#!/usr/bin/env node` loader: small, no native magic, and perfectly
        // healthy. `cheap_reject` answers "can this be executed at all", and
        // for this file the answer is yes. Anything more is the probe's call.
        let dir = tempfile::tempdir().unwrap();
        let shim = write_executable(
            dir.path(),
            "copilot",
            b"#!/usr/bin/env node\nrequire('@github/copilot/npm-loader.js');\n",
        );
        assert_eq!(
            cheap_reject(&shim),
            None,
            "a small executable file is not a rejection — size is not evidence"
        );
    }

    #[cfg(unix)]
    #[test]
    fn cheap_reject_reports_only_filesystem_facts() {
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            cheap_reject(&dir.path().join("nothing-here")),
            Some(Rejection::Missing)
        );
        assert_eq!(cheap_reject(dir.path()), Some(Rejection::NotAFile));

        let path = dir.path().join("not-executable");
        std::fs::write(&path, b"#!/bin/sh\nexit 0\n").unwrap();
        assert_eq!(cheap_reject(&path), Some(Rejection::NotExecutable));
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_probe_on_the_real_stub_is_labelled_a_placeholder() {
        // The good diagnosis the removed fast path was written for, kept: an
        // incomplete claude install still says "incomplete install", it just
        // says it after the probe has already rejected the file instead of
        // before anyone looked.
        let dir = tempfile::tempdir().unwrap();
        let stub = write_executable(dir.path(), "claude", &real_stub_bytes());
        assert_eq!(label_failed_probe(&stub), Rejection::PlaceholderStub);
    }

    #[cfg(unix)]
    #[test]
    fn a_failed_probe_on_something_substantial_stays_a_plain_probe_failure() {
        let dir = tempfile::tempdir().unwrap();
        let big = write_executable(
            dir.path(),
            "claude",
            &vec![b'#'; crate::claude_native::STUB_MAX_LEN as usize + 1],
        );
        assert_eq!(label_failed_probe(&big), Rejection::ProbeFailed);
    }

    #[test]
    fn an_unreadable_candidate_is_not_diagnosed_as_a_placeholder() {
        // The old code read the head with `.unwrap_or(0)`, so an EACCES became
        // `read == 0` and every file under 4 KiB was confidently reported as
        // "incomplete install — this is the small placeholder…". An I/O error
        // is not evidence about contents nobody managed to read.
        let dir = tempfile::tempdir().unwrap();
        assert_eq!(
            label_failed_probe(&dir.path().join("vanished")),
            Rejection::Unreadable
        );
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
    // decide_install — the whole of Defect 2, as a table
    // ------------------------------------------------------------------

    fn target(source: TargetSource, version: &str) -> LaunchTarget {
        LaunchTarget {
            path: PathBuf::from("/anywhere/claude"),
            version: version.to_string(),
            source,
        }
    }

    fn resolved(source: TargetSource, version: &str) -> Resolution {
        Resolution {
            target: Some(target(source, version)),
            rejected: Vec::new(),
        }
    }

    fn nothing_resolved(rejected: &[Rejection]) -> Resolution {
        Resolution {
            target: None,
            rejected: rejected
                .iter()
                .enumerate()
                .map(|(i, r)| (PathBuf::from(format!("/candidate/{i}/claude")), *r))
                .collect(),
        }
    }

    #[test]
    fn decide_install_installs_when_nothing_healthy_exists() {
        for latest in [Some("2.1.238"), None] {
            assert_eq!(
                decide_install(&nothing_resolved(&[Rejection::PlaceholderStub]), latest),
                InstallDecision::InstallMissing
            );
        }
        assert_eq!(
            decide_install(&Resolution::default(), None),
            InstallDecision::InstallMissing,
            "an empty candidate list is still 'nothing is installed'"
        );
    }

    #[test]
    fn decide_install_abstains_when_a_candidate_timed_out() {
        // A 3 s `--version` timeout on a loaded box is the same class of
        // transient as a failed registry query, and the rule one paragraph up
        // in the docstring already says a transient must not cost ~339 MB.
        // Before this, `ProbeTimedOut` was indistinguishable from "nothing is
        // installed" and bought a full reinstall.
        assert_eq!(
            decide_install(
                &nothing_resolved(&[Rejection::ProbeTimedOut]),
                Some("2.1.238")
            ),
            InstallDecision::Abstain
        );
    }

    #[test]
    fn decide_install_abstains_if_any_candidate_timed_out() {
        // One inconclusive candidate is enough: the binary that would have
        // answered may be the one that hung.
        assert_eq!(
            decide_install(
                &nothing_resolved(&[
                    Rejection::Missing,
                    Rejection::ProbeTimedOut,
                    Rejection::PlaceholderStub,
                ]),
                Some("2.1.238"),
            ),
            InstallDecision::Abstain
        );
    }

    #[test]
    fn decide_install_still_installs_when_every_rejection_is_conclusive() {
        // Missing / not executable / a stub / a non-zero probe all mean "there
        // is no working binary here", which is precisely what an install fixes.
        assert_eq!(
            decide_install(
                &nothing_resolved(&[
                    Rejection::Missing,
                    Rejection::NotAFile,
                    Rejection::NotExecutable,
                    Rejection::PlaceholderStub,
                    Rejection::Unreadable,
                    Rejection::ProbeFailed,
                    Rejection::UnparseableVersion,
                ]),
                Some("2.1.238"),
            ),
            InstallDecision::InstallMissing
        );
    }

    #[test]
    fn decide_install_ignores_a_timeout_once_something_healthy_was_found() {
        // A hung candidate ahead of a healthy one is not inconclusive: we know
        // what we are going to launch.
        let mut resolution = resolved(TargetSource::Path, "2.1.237");
        resolution
            .rejected
            .push((PathBuf::from("/slow/claude"), Rejection::ProbeTimedOut));
        assert_eq!(
            decide_install(&resolution, Some("2.1.238")),
            InstallDecision::UseExisting
        );
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
                decide_install(&resolved(source, "2.1.237"), Some("2.1.238")),
                InstallDecision::UseExisting,
                "must not upgrade a non-owned target ({source:?})"
            );
        }
    }

    #[test]
    fn decide_install_upgrades_a_stale_binary_in_amplihacks_own_prefix() {
        assert_eq!(
            decide_install(
                &resolved(TargetSource::AmplihackPrefix, "2.1.237"),
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
                &resolved(TargetSource::AmplihackPrefix, "2.1.238"),
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
            decide_install(&resolved(TargetSource::AmplihackPrefix, "2.1.237"), None),
            InstallDecision::UseExisting
        );
    }

    // ------------------------------------------------------------------
    // rejection_report — Defect 3's message contract (A-AMB-11)
    // ------------------------------------------------------------------

    const CLAUDE_PKG: &str = "@anthropic-ai/claude-code";

    fn stub_and_timeout_resolution() -> Resolution {
        Resolution {
            target: None,
            rejected: vec![
                (
                    PathBuf::from("/home/you/.npm-global/bin/claude"),
                    Rejection::PlaceholderStub,
                ),
                (
                    PathBuf::from("/home/you/.local/bin/claude"),
                    Rejection::ProbeTimedOut,
                ),
            ],
        }
    }

    fn claude_report(resolution: &Resolution) -> String {
        resolution.rejection_report("claude", CLAUDE_PKG)
    }

    #[test]
    fn rejection_report_names_the_real_cause() {
        let report = claude_report(&stub_and_timeout_resolution()).to_lowercase();
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
        let report = claude_report(&stub_and_timeout_resolution());
        assert!(
            report.contains("npm install") && report.contains(CLAUDE_PKG),
            "must state a copy-pasteable remedy, got:\n{report}"
        );
    }

    #[test]
    fn rejection_report_names_the_tool_and_package_it_was_given() {
        // The regression this signature exists to prevent: a copilot user was
        // told "No usable claude binary was found" and instructed to install
        // @anthropic-ai/claude-code.
        let report = stub_and_timeout_resolution().rejection_report("copilot", "@github/copilot");
        assert!(
            report.contains("copilot") && report.contains("@github/copilot"),
            "the report must speak about the tool it was asked about, got:\n{report}"
        );
        assert!(
            !report.contains("claude-code") && !report.to_lowercase().contains("claude binary"),
            "a copilot failure must not name claude's package, got:\n{report}"
        );
    }

    #[test]
    fn rejection_report_lists_every_rejected_candidate() {
        let report = claude_report(&stub_and_timeout_resolution());
        assert!(report.contains("/home/you/.npm-global/bin/claude"));
        assert!(report.contains("/home/you/.local/bin/claude"));
    }

    #[test]
    fn rejection_report_does_not_claim_nothing_was_found_when_something_was() {
        // The spawn-failure path (`launch/mod.rs`) reports a target that WAS
        // resolved and then would not exec. The old headline said "No usable
        // claude binary was found" over a list that did not contain it and was
        // usually empty — false, and it hid the one path that matters.
        let resolution = resolved(TargetSource::Path, "2.1.238");
        let report = claude_report(&resolution);
        assert!(
            !report.to_lowercase().contains("no usable"),
            "something WAS found; the report must not say otherwise:\n{report}"
        );
        assert!(
            report.contains("/anywhere/claude") && report.contains("2.1.238"),
            "the report must name the binary that failed and its version:\n{report}"
        );
    }

    #[test]
    fn rejection_report_says_nothing_was_found_when_nothing_was() {
        let report = claude_report(&stub_and_timeout_resolution()).to_lowercase();
        assert!(
            report.contains("no usable claude binary was found"),
            "got:\n{report}"
        );
    }

    #[test]
    fn rejection_report_does_not_send_the_user_hunting_for_an_arch_problem() {
        // The old message was `Exec format error (os error 8)`, which named
        // nothing real and pointed at a CPU-architecture problem that did not
        // exist.
        let report = claude_report(&stub_and_timeout_resolution()).to_lowercase();
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
                Rejection::PlaceholderStub,
            )],
        };
        let report = claude_report(&resolution);
        assert!(
            !report.contains('\x1b'),
            "no ESC may reach the TTY, got: {report:?}"
        );
    }

    #[test]
    fn a_newline_in_a_candidate_path_cannot_forge_a_report_row() {
        // The report renders "\n  {path}\n      {reason}\n". A $PATH entry
        // carrying a newline could otherwise inject convincing extra rows —
        // making an attacker's rejected candidate read as a healthy one.
        let resolution = Resolution {
            target: None,
            rejected: vec![(
                PathBuf::from("/tmp/a\n  /usr/bin/claude\n      ok"),
                Rejection::PlaceholderStub,
            )],
        };
        let report = claude_report(&resolution);
        let rows = report.lines().filter(|l| l.starts_with("  /")).count();
        assert_eq!(
            rows, 1,
            "exactly one candidate row may render, got:\n{report}"
        );
    }

    #[test]
    fn rejection_report_carries_no_environment() {
        // Error text carries paths, reasons, and the remedy. Nothing else.
        let report = claude_report(&stub_and_timeout_resolution());
        for leak in ["PATH=", "HOME=", "AMPLIHACK_", "NODE_OPTIONS"] {
            assert!(
                !report.contains(leak),
                "report must not leak {leak:?}, got:\n{report}"
            );
        }
    }
}
