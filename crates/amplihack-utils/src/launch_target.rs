//! The single health-gated resolver for agent-tool binaries.
//!
//! # Why this module exists
//!
//! Before this module, amplihack had **four independent** notions of "the
//! claude binary" and four independent computations of the npm prefix. A
//! single launch on the azlin "dev" VM (2026-08-21) demonstrated three of
//! them disagreeing at once:
//!
//! ```text
//! amplihack: update available: @anthropic-ai/claude-code 2.1.237 -> 2.1.238
//! Installing claude via npm package @anthropic-ai/claude-code...
//! INFO launching claude binary=/home/azureuser/.local/bin/claude version="2.1.238"
//! ```
//!
//! - **version-checked**: `/usr/bin/claude` (via `npm list -g`, which with no
//!   `--prefix` resolves to `/usr`) — decided "upgrade needed"
//! - **installed to**: `~/.npm-global/bin/claude` — received a ~500-byte stub
//! - **launched**: `~/.local/bin/claude` — the only working one, and only by
//!   the accident of PATH ordering
//!
//! Because the check never read what the install wrote, the check was
//! *permanently* stale: every launch re-downloaded 339MB, every launch
//! reinstalled the stub, and any hand-repair was clobbered on the next run.
//!
//! # The contract
//!
//! Whatever decides what to `exec` is what decides whether an upgrade is
//! needed and where it lands. [`resolve`] is that one function. Three call
//! sites consume it: the update check, the install target selection, and the
//! exec site.
//!
//! Two invariants carry the fix:
//!
//! 1. **Health gating.** [`Resolution::selected`] is `Some` only for a
//!    candidate whose [`Health`] is [`Health::Working`] — i.e. one that
//!    structurally looks like a native binary *and* answered `--version`. A
//!    binary whose probe fails, times out, or returns nothing is a **failed
//!    install** and is never exec'd. `version="unknown"` is unrepresentable.
//! 2. **Ownership is authorization.** amplihack installs over, repairs, and
//!    deletes only binaries it put there itself (see [`is_amplihack_owned`]).
//!    A stale binary it does not own gets [`LaunchAction::NoticeOnly`] — a
//!    printed notice and nothing else. Installing there is what created the
//!    shadow copy at a different precedence in the first place.
//!
//! `resolve` is infallible by design: absence, unreadable PATH entries, probe
//! failures and timeouts are all *data* in [`Resolution::rejected`], never an
//! `Err`. Diagnostics render from that list via [`render_rejections`].
//!
//! See `docs/LAUNCH_TARGET_RESOLUTION.md` for the full contract.

use crate::binary_finder::{self, VersionProbe};
use std::collections::HashSet;
use std::env;
use std::path::{Path, PathBuf};
use std::sync::LazyLock;
use std::time::Duration;

// ---------------------------------------------------------------------------
// Tunables
// ---------------------------------------------------------------------------

/// Maximum number of candidates whose `--version` we will actually execute in
/// a single launch.
///
/// SEC-A14: the scan continues past rejected candidates, so without a cap the
/// worst case is `VALIDATION_PROBE_TIMEOUT × N` subprocesses on the launch
/// path, with `N` set by the length of the user's `$PATH`. Candidates are
/// de-duplicated by fully-resolved canonical path before being counted.
pub const MAX_PROBE_CANDIDATES: usize = 8;

/// Timeout for the *validation* probe (post-install and pre-exec).
///
/// Deliberately 20× `binary_finder`'s 500ms discovery timeout. A 339MB
/// binary's cold first run does not finish in 500ms, and under this module's
/// rules a false "unknown" means a **rejected install** — so the tight
/// discovery budget would turn a slow disk into a spurious reinstall loop.
/// This probe runs at most [`MAX_PROBE_CANDIDATES`] times per launch, and the
/// zero-subprocess structural filter below short-circuits the failure mode
/// that actually occurs in practice.
pub const VALIDATION_PROBE_TIMEOUT: Duration = Duration::from_secs(10);

/// Files smaller than this that lack native-executable magic are treated as
/// stubs. The real `@anthropic-ai/claude-code` placeholder is ~500 bytes; the
/// real binary is ~339MB.
const STUB_SIZE_THRESHOLD: u64 = 4096;

/// Cap on rendered candidate paths, applied after control-character stripping.
const MAX_DISPLAY_PATH_LEN: usize = 256;

// ---------------------------------------------------------------------------
// Model
// ---------------------------------------------------------------------------

/// Where a candidate binary came from. Also the precedence order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Source {
    /// `AMPLIHACK_{TOOL}_BINARY_PATH` or `{TOOL}_BINARY_PATH`.
    ///
    /// User-directed, therefore never amplihack-owned for the purposes of
    /// mutation: amplihack does not install over, repair, or delete a path the
    /// user named explicitly, even if it happens to sit inside amplihack's own
    /// prefix.
    EnvOverride,
    /// Found by scanning `$PATH`.
    Path,
    /// Found in a known install directory that may not be on `$PATH`
    /// (`~/.npm-global/bin`, `~/.cargo/bin`, `~/.local/bin`).
    FallbackDir,
}

/// Whether amplihack installed this binary — the sole authorization signal for
/// mutation.
///
/// This is the cached output of [`is_amplihack_owned`]. It is stored on
/// [`Candidate`] rather than recomputed at each use site on purpose: a second
/// implementation of the containment check is a second place for the
/// authorization model to be wrong.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Ownership {
    /// Lives inside amplihack's own npm prefix — amplihack may install over,
    /// repair, or remove it.
    AmplihackOwned,
    /// Anything else. Read and exec only; never written, never deleted.
    External,
}

/// Why a candidate was rejected. Each variant maps to exactly one remedy line
/// in [`render_rejections`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BrokenReason {
    /// Too small to be the native binary and carrying no executable magic —
    /// the npm placeholder, or a script masquerading as the tool.
    Stub,
    /// Present but missing the execute bit.
    NotExecutable,
    /// `--version` failed to run, exited non-zero, or printed nothing.
    ProbeFailed,
    /// `--version` did not return within [`VALIDATION_PROBE_TIMEOUT`].
    ProbeTimedOut,
}

/// The result of health-gating a candidate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Health {
    /// Structurally sound and answered `--version`.
    Working {
        /// The raw first line of `--version`, ANSI-stripped and length-capped.
        ///
        /// Never print this directly — it is untrusted output from a binary on
        /// `$PATH`. Print [`Health::Working::semver`] instead (SEC-A15).
        version: String,
        /// [`extract_semver`] of `version`, if it contains one.
        ///
        /// `None` is normal (dev builds, forks) and **fails closed**: no
        /// semver means no comparison, which means no upgrade.
        semver: Option<String>,
    },
    /// Rejected. Never selected, never exec'd.
    Broken(BrokenReason),
}

/// One discovered binary and everything decided about it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Candidate {
    /// Absolute, directory-normalized path.
    ///
    /// The parent directory is canonicalized (so two `$PATH` entries reaching
    /// the same directory compare equal) but the final component is **not**
    /// followed, so a symlinked bin entry stays addressable as the link. This
    /// is the exact path that gets exec'd (SEC-A23).
    pub path: PathBuf,
    /// Which lookup produced it.
    pub source: Source,
    /// Health-gate verdict.
    pub health: Health,
    /// Cached [`is_amplihack_owned`] result.
    pub ownership: Ownership,
}

/// The outcome of a resolution pass.
///
/// Contract: `selected.is_some()` implies its `health` is [`Health::Working`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Resolution {
    /// The binary to launch, if any candidate passed the health gate.
    pub selected: Option<Candidate>,
    /// Every candidate considered and not selected, with its reason.
    pub rejected: Vec<Candidate>,
}

/// Launch-time facts that are not properties of the binary itself.
///
/// A struct rather than two adjacent bare `bool` parameters, which are trivial
/// to transpose at a call site and impossible to catch by type.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LaunchContext {
    /// Whether this tool can be installed/upgraded from npm at all.
    pub npm_backed: bool,
    /// Whether amplihack may print notices and spend time installing.
    pub interactive: bool,
}

/// What the launch path should do about the resolved target.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum LaunchAction {
    /// Exec the selection as-is.
    Launch,
    /// Nothing healthy exists; install from scratch.
    InstallFresh,
    /// The selection is amplihack's own and is stale; reinstall in place.
    Upgrade {
        /// Currently installed semver.
        from: String,
        /// Latest published semver.
        to: String,
    },
    /// The selection is stale but amplihack does not own it. Print a notice
    /// and launch it anyway.
    ///
    /// **This arm is the Defect-2 fix.** Installing here would write to a
    /// *different* directory than the one that was checked, creating a second
    /// copy at a different `$PATH` precedence — which is exactly how the
    /// version check ended up permanently reading a binary nobody launched.
    NoticeOnly {
        /// Currently installed semver.
        from: String,
        /// Latest published semver.
        to: String,
    },
    /// Nothing healthy, and no way to install. Fail loudly rather than exec
    /// something known-broken.
    Fail,
}

/// What to do about a broken binary amplihack may own.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RepairAction {
    /// Leave it alone.
    None,
    /// Re-run the install to materialize what is missing.
    CompleteInstall,
    /// Repair already failed once; remove the file so it stops shadowing a
    /// working binary further down `$PATH`.
    Purge,
}

/// Result of a purge attempt.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PurgeOutcome {
    /// The file (or symlink) was removed.
    Removed,
    /// Refused — the path is not inside amplihack's prefix, or the prefix is
    /// unresolvable. Deny-by-default.
    Denied,
    /// Authorized, but the removal itself failed.
    Failed,
}

// ---------------------------------------------------------------------------
// Version parsing
// ---------------------------------------------------------------------------

/// Matches a bare semver, optionally with a pre-release suffix.
static SEMVER_RE: LazyLock<regex::Regex> = LazyLock::new(|| {
    regex::Regex::new(r"\d+\.\d+\.\d+(?:-[0-9A-Za-z.-]+)?").expect("static semver regex")
});

/// Pull a bare semver out of arbitrary `--version` output.
///
/// # Why not `sanitize_version`
///
/// `sanitize_version` filters to an allowlist of characters *in place*, so
/// `claude --version`'s actual output —
///
/// ```text
/// 2.1.238 (Claude Code)
/// ```
///
/// — becomes `2.1.238ClaudeCode`, which never compares equal to npm's
/// `2.1.238`. That was a second, independent always-stale loop stacked on top
/// of the wrong-binary one: even had the check read the right binary, it would
/// still have concluded "upgrade needed" forever.
///
/// # Security
///
/// This doubles as the display allowlist (SEC-A15). `strip_ansi` handles CSI
/// sequences only and passes OSC (`ESC ]`), `ESC c`, DCS, and a bare `CR`
/// straight through. Matching a semver shape and returning *only* the match
/// admits no control bytes at all, by construction.
///
/// Returns `None` when there is no semver — which fails closed, since no
/// version means no upgrade comparison.
///
/// # Examples
///
/// ```
/// use amplihack_utils::launch_target::extract_semver;
///
/// assert_eq!(extract_semver("2.1.238 (Claude Code)").as_deref(), Some("2.1.238"));
/// assert_eq!(extract_semver("v1.0.0-beta.3").as_deref(), Some("1.0.0-beta.3"));
/// assert_eq!(extract_semver("Error: claude native binary not installed."), None);
/// ```
pub fn extract_semver(text: &str) -> Option<String> {
    let matched = SEMVER_RE.find(text)?.as_str();
    let trimmed = matched.trim_end_matches(['.', '-']);
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

// ---------------------------------------------------------------------------
// The npm prefix — one definition
// ---------------------------------------------------------------------------

/// A prefix is usable only if it is absolute and at least two components deep.
///
/// SEC-A5, deny-by-default: this predicate gates *deletion*. `HOME=/` would
/// yield `/.npm-global`, one component below the filesystem root; refuse to
/// treat anything that shallow as amplihack's own territory rather than
/// authorizing removals near `/`.
fn is_safe_prefix(prefix: &Path) -> bool {
    prefix.is_absolute()
        && prefix
            .components()
            .filter(|c| matches!(c, std::path::Component::Normal(_)))
            .count()
            >= 2
}

/// Compute amplihack's npm prefix from an explicit home directory.
///
/// Split from [`npm_prefix_dir`] so the deny-by-default rules are testable
/// without mutating the process environment.
///
/// Returns `None` for an unset, empty, relative, or too-shallow home.
///
/// # Examples
///
/// ```
/// use amplihack_utils::launch_target::npm_prefix_dir_from;
/// use std::path::{Path, PathBuf};
///
/// assert_eq!(
///     npm_prefix_dir_from(Some(Path::new("/home/u"))),
///     Some(PathBuf::from("/home/u/.npm-global"))
/// );
/// assert_eq!(npm_prefix_dir_from(None), None);
/// ```
pub fn npm_prefix_dir_from(home: Option<&Path>) -> Option<PathBuf> {
    let home = home?;
    if home.as_os_str().is_empty() || !home.is_absolute() {
        return None;
    }
    let prefix = home.join(".npm-global");
    if !is_safe_prefix(&prefix) {
        return None;
    }
    Some(prefix)
}

/// The single definition of amplihack's npm prefix.
///
/// Four call sites used to compute `~/.npm-global` independently
/// (`bootstrap.rs`, `binary_finder.rs`, `claude_cli.rs`, `launch/command.rs`).
/// They now all read through here, so they cannot drift.
pub fn npm_prefix_dir() -> Option<PathBuf> {
    let home = env::var_os("HOME")
        .map(PathBuf::from)
        .or_else(|| env::var_os("USERPROFILE").map(PathBuf::from))?;
    npm_prefix_dir_from(Some(&home))
}

/// The `bin` directory under [`npm_prefix_dir`].
pub fn npm_bin_dir() -> Option<PathBuf> {
    npm_prefix_dir().map(|p| p.join("bin"))
}

// ---------------------------------------------------------------------------
// Ownership — the sole authorization predicate
// ---------------------------------------------------------------------------

/// Does `path` live inside `prefix`?
///
/// This is the **only** predicate that authorizes mutation: install-over,
/// repair, delete, and child-`PATH` promotion all gate on it. There must not
/// be a second implementation anywhere in the tree.
///
/// # Implementation notes that are load-bearing
///
/// - **Component-wise, never string-prefix.** `~/.npm-global-backup/bin/claude`
///   is a *string* prefix match on `~/.npm-global` and a component-wise
///   non-match. Getting this wrong authorizes amplihack to delete files in a
///   directory it never created.
/// - **Canonicalize the parent directory, not the symlink target.** Ownership
///   answers "does this link live in our prefix"; [`probe_health`] separately
///   answers "is its target a stub". Collapsing the two lets a hostile symlink
///   whose target points back into the prefix authorize itself.
/// - **Deny on every error.** Unresolvable prefix, missing path, unreadable
///   parent: all `false`.
pub fn is_amplihack_owned_under(prefix: Option<&Path>, path: &Path) -> bool {
    let Some(prefix) = prefix else {
        return false;
    };
    if !is_safe_prefix(prefix) {
        return false;
    }
    // The path itself must exist. `symlink_metadata` deliberately does not
    // follow the link — a dangling link is still a real directory entry.
    if path.symlink_metadata().is_err() {
        return false;
    }
    let Some(parent) = path.parent() else {
        return false;
    };
    let Ok(parent_canon) = std::fs::canonicalize(parent) else {
        return false;
    };
    let Ok(prefix_canon) = std::fs::canonicalize(prefix) else {
        return false;
    };
    parent_canon.starts_with(&prefix_canon)
}

/// [`is_amplihack_owned_under`] against the ambient [`npm_prefix_dir`].
pub fn is_amplihack_owned(path: &Path) -> bool {
    is_amplihack_owned_under(npm_prefix_dir().as_deref(), path)
}

/// [`Ownership`] for `path`, resolved against the ambient prefix.
pub fn ownership_of(path: &Path) -> Ownership {
    if is_amplihack_owned(path) {
        Ownership::AmplihackOwned
    } else {
        Ownership::External
    }
}

// ---------------------------------------------------------------------------
// The health gate
// ---------------------------------------------------------------------------

/// Native-executable magic numbers, checked before any subprocess is spawned.
fn has_native_executable_magic(path: &Path) -> bool {
    use std::io::Read;
    let Ok(mut file) = std::fs::File::open(path) else {
        return false;
    };
    let mut head = [0u8; 4];
    let Ok(read) = file.read(&mut head) else {
        return false;
    };
    let head = &head[..read];
    // ELF
    if head.starts_with(b"\x7fELF") {
        return true;
    }
    // PE / DOS stub
    if head.starts_with(b"MZ") {
        return true;
    }
    // Mach-O thin (both endiannesses, 32- and 64-bit) and fat/universal.
    matches!(
        head,
        [0xfe, 0xed, 0xfa, 0xce]
            | [0xfe, 0xed, 0xfa, 0xcf]
            | [0xce, 0xfa, 0xed, 0xfe]
            | [0xcf, 0xfa, 0xed, 0xfe]
            | [0xca, 0xfe, 0xba, 0xbe]
            | [0xbe, 0xba, 0xfe, 0xca]
    )
}

/// Health-gate a single candidate.
///
/// # Ordering is a security control (SEC-A12)
///
/// The structural filter runs **first and spawns nothing**. That is what stops
/// a hostile ~500-byte script sitting in `$PATH[0]` from being executed at
/// all. Only a candidate that already looks like a native binary earns the
/// right to have its `--version` run. Do not reorder these steps, and do not
/// drop the structural filter as an "optimization" — it is the thing standing
/// between an untrusted `$PATH` entry and a subprocess.
///
/// A timeout is treated identically to a failure: such a binary is not exec'd.
pub fn probe_health(path: &Path) -> Health {
    // Resolve the symlink chain once. The real stub is a symlink from
    // `<prefix>/bin/claude` into
    // `lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe`, so size and
    // magic checks must target the *resolved* file, not the link.
    let Ok(resolved) = std::fs::canonicalize(path) else {
        return Health::Broken(BrokenReason::ProbeFailed);
    };

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let Ok(meta) = std::fs::metadata(&resolved) else {
            return Health::Broken(BrokenReason::ProbeFailed);
        };
        // Step 1 — zero-subprocess structural filter.
        if meta.len() < STUB_SIZE_THRESHOLD && !has_native_executable_magic(&resolved) {
            return Health::Broken(BrokenReason::Stub);
        }
        // Step 2 — still no subprocess.
        if meta.permissions().mode() & 0o111 == 0 {
            return Health::Broken(BrokenReason::NotExecutable);
        }
    }
    #[cfg(not(unix))]
    {
        // Windows shims are small text files (`.cmd`, `.ps1`), so the size
        // heuristic would reject working installs. The probe is the gate here.
        if !resolved.is_file() {
            return Health::Broken(BrokenReason::ProbeFailed);
        }
    }

    // Step 3 — only now do we run it.
    match binary_finder::detect_version_with_timeout(path, VALIDATION_PROBE_TIMEOUT) {
        VersionProbe::TimedOut => Health::Broken(BrokenReason::ProbeTimedOut),
        VersionProbe::Failed => Health::Broken(BrokenReason::ProbeFailed),
        VersionProbe::Version(raw) => {
            let version = raw.trim().to_string();
            if version.is_empty() {
                // A binary that exits 0 and prints nothing told us nothing.
                // "unknown" is a failed install, not a launchable state.
                return Health::Broken(BrokenReason::ProbeFailed);
            }
            let semver = extract_semver(&version);
            Health::Working { version, semver }
        }
    }
}

// ---------------------------------------------------------------------------
// Resolution
// ---------------------------------------------------------------------------

/// Normalize a candidate to an absolute path with a canonical parent.
///
/// The final component is deliberately *not* resolved: a symlinked bin entry
/// must stay addressable as the link so a purge removes the link rather than
/// whatever it points at.
fn normalize_entry(entry: &Path) -> PathBuf {
    match (entry.parent(), entry.file_name()) {
        (Some(parent), Some(name)) => match std::fs::canonicalize(parent) {
            Ok(canon) => canon.join(name),
            Err(_) => entry.to_path_buf(),
        },
        _ => entry.to_path_buf(),
    }
}

/// Build the ordered candidate list. Precedence is unchanged from
/// `BinaryFinder::find`: env override, then `$PATH`, then known install dirs,
/// with the tool's alternate binary names taking the outer loop.
fn candidate_entries(tool: &str) -> Vec<(PathBuf, Source)> {
    let mut entries = Vec::new();
    let tool_upper = tool.to_uppercase();

    for key in [
        format!("AMPLIHACK_{tool_upper}_BINARY_PATH"),
        format!("{tool_upper}_BINARY_PATH"),
    ] {
        if let Some(value) = env::var_os(&key) {
            let path = PathBuf::from(value);
            if !path.as_os_str().is_empty() {
                entries.push((path, Source::EnvOverride));
            }
        }
    }

    let names = binary_finder::binary_candidates(tool);
    let path_dirs = binary_finder::search_path_dirs();
    let fallback_dirs = binary_finder::install_fallback_dirs();
    for name in &names {
        for dir in &path_dirs {
            entries.push((dir.join(name), Source::Path));
        }
        for dir in &fallback_dirs {
            if !path_dirs.contains(dir) {
                entries.push((dir.join(name), Source::FallbackDir));
            }
        }
    }
    entries
}

/// Resolve the binary amplihack should launch for `tool`.
///
/// Infallible: nothing here returns `Err`. A candidate that is absent,
/// unreadable, structurally a stub, or fails its version probe becomes an
/// entry in [`Resolution::rejected`] carrying its reason. That list is what
/// [`render_rejections`] turns into an actionable diagnostic.
///
/// Scanning stops at the first healthy candidate, and at most
/// [`MAX_PROBE_CANDIDATES`] binaries are ever executed.
pub fn resolve(tool: &str) -> Resolution {
    let prefix = npm_prefix_dir();
    let mut resolution = Resolution {
        selected: None,
        rejected: Vec::new(),
    };
    let mut seen: HashSet<PathBuf> = HashSet::new();
    let mut probed = 0usize;

    for (entry, source) in candidate_entries(tool) {
        if probed >= MAX_PROBE_CANDIDATES {
            tracing::debug!(
                tool,
                cap = MAX_PROBE_CANDIDATES,
                "probe budget exhausted; stopping candidate scan"
            );
            break;
        }
        if !entry.is_file() {
            continue;
        }
        // De-duplicate on the FULLY resolved path so two `$PATH` entries that
        // reach the same inode are probed once (SEC-A14).
        let Ok(dedupe_key) = std::fs::canonicalize(&entry) else {
            continue;
        };
        if !seen.insert(dedupe_key) {
            continue;
        }

        // Ownership is computed from the ENTRY path, before normalization, so
        // the parent directory under test is the one the link lives in.
        let ownership = if is_amplihack_owned_under(prefix.as_deref(), &entry) {
            Ownership::AmplihackOwned
        } else {
            Ownership::External
        };

        let path = normalize_entry(&entry);
        probed += 1;
        let health = probe_health(&path);
        let is_working = matches!(health, Health::Working { .. });
        let candidate = Candidate {
            path,
            source,
            health,
            ownership,
        };

        if is_working {
            resolution.selected = Some(candidate);
            break;
        }
        tracing::debug!(
            tool,
            path = %candidate.path.display(),
            health = ?candidate.health,
            "candidate rejected by the health gate"
        );
        resolution.rejected.push(candidate);
    }

    resolution
}

// ---------------------------------------------------------------------------
// Decisions — pure, no I/O
// ---------------------------------------------------------------------------

/// Decide what the launch path should do, given a resolution and the latest
/// published version.
///
/// Pure: no filesystem, no network, no subprocesses. All of Defect 2's fix
/// lives here and is therefore testable without touching a disk.
///
/// Fails closed at every uncertainty:
/// - no healthy selection and no npm backing → [`LaunchAction::Fail`]
/// - registry unreachable (`latest` is `None`) → [`LaunchAction::Launch`]
/// - selection has no parseable semver → [`LaunchAction::Launch`]
/// - selection is stale but not amplihack's → [`LaunchAction::NoticeOnly`]
pub fn decide_launch_action(
    resolution: &Resolution,
    latest: Option<&str>,
    ctx: LaunchContext,
) -> LaunchAction {
    let Some(selected) = resolution.selected.as_ref() else {
        // Nothing healthy exists. Installing is the only path forward, and it
        // must happen even non-interactively or a fresh host can never launch.
        return if ctx.npm_backed {
            LaunchAction::InstallFresh
        } else {
            LaunchAction::Fail
        };
    };

    if !ctx.npm_backed || !ctx.interactive {
        return LaunchAction::Launch;
    }
    let Some(latest) = latest else {
        // Registry unreachable must never trigger an install.
        return LaunchAction::Launch;
    };
    let Health::Working {
        semver: Some(installed),
        ..
    } = &selected.health
    else {
        // No comparable version => no comparison => no upgrade. Never
        // "upgrade because we couldn't tell", which is the reinstall-forever
        // loop in its purest form.
        return LaunchAction::Launch;
    };
    if installed == latest {
        return LaunchAction::Launch;
    }

    // Stale. Whether we may do anything about it is an ownership question.
    let may_install =
        selected.ownership == Ownership::AmplihackOwned && selected.source != Source::EnvOverride;
    if may_install {
        LaunchAction::Upgrade {
            from: installed.clone(),
            to: latest.to_string(),
        }
    } else {
        LaunchAction::NoticeOnly {
            from: installed.clone(),
            to: latest.to_string(),
        }
    }
}

/// Decide whether a broken binary may be repaired or removed.
///
/// Pure and **total**: no I/O and no path arithmetic. Containment already
/// happened — the caller passes a precomputed [`Ownership`] — so the
/// authorization logic lives in exactly one audited place
/// ([`is_amplihack_owned_under`]) and the deny arms here are unit-testable
/// with no filesystem at all.
///
/// Deny-by-default on every path that is not explicitly allowed.
pub fn decide_repair_action(
    ownership: Ownership,
    health: &Health,
    source: Source,
    repair_already_attempted: bool,
) -> RepairAction {
    // Never touch a binary amplihack did not install.
    if ownership != Ownership::AmplihackOwned {
        return RepairAction::None;
    }
    // SEC-A5: a path the user named explicitly is left alone, full stop —
    // even when it happens to sit inside amplihack's prefix.
    if source == Source::EnvOverride {
        return RepairAction::None;
    }
    match health {
        Health::Working { .. } => RepairAction::None,
        // Purge is the second step, never the first: try to make the install
        // whole before removing anything.
        Health::Broken(_) if repair_already_attempted => RepairAction::Purge,
        Health::Broken(_) => RepairAction::CompleteInstall,
    }
}

// ---------------------------------------------------------------------------
// Destructive operation — the only one on the launch path
// ---------------------------------------------------------------------------

/// Remove a binary amplihack owns, after re-checking containment.
///
/// [`decide_repair_action`] decides *whether*; this decides *may we*, and the
/// containment check is repeated here rather than trusted from the caller.
///
/// - `symlink_metadata` + `remove_file` deletes the **link**, never its
///   target. The stub is a symlink into `lib/node_modules/`; a
///   follow-then-delete would destroy whatever it points at.
/// - Anything outside `prefix`, and any unresolvable `prefix`, is
///   [`PurgeOutcome::Denied`].
pub fn purge_binary_under(prefix: Option<&Path>, path: &Path) -> PurgeOutcome {
    if !is_amplihack_owned_under(prefix, path) {
        tracing::warn!(
            path = %sanitize_display_path(path),
            "refusing to remove a binary outside amplihack's own prefix"
        );
        return PurgeOutcome::Denied;
    }
    if std::fs::symlink_metadata(path).is_err() {
        return PurgeOutcome::Denied;
    }
    match std::fs::remove_file(path) {
        Ok(()) => {
            tracing::info!(
                path = %sanitize_display_path(path),
                "removed a non-functional binary from amplihack's prefix"
            );
            PurgeOutcome::Removed
        }
        Err(err) => {
            tracing::warn!(%err, path = %sanitize_display_path(path), "purge failed");
            PurgeOutcome::Failed
        }
    }
}

/// [`purge_binary_under`] against the ambient [`npm_prefix_dir`].
pub fn purge_binary(path: &Path) -> PurgeOutcome {
    purge_binary_under(npm_prefix_dir().as_deref(), path)
}

// ---------------------------------------------------------------------------
// Diagnostics
// ---------------------------------------------------------------------------

/// Strip control characters from a path before printing it.
///
/// SEC-A16: a filename in a writable `$PATH` directory is attacker-influenceable
/// and is sanitized nowhere else. `strip_ansi` handles CSI sequences only — it
/// does not touch OSC (`ESC ]`), `ESC c`, DCS, or a bare `CR`. Dropping every
/// control character covers all of them without needing to enumerate escape
/// grammars.
pub fn sanitize_display_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    let mut out: String = raw.chars().filter(|c| !c.is_control()).collect();
    if out.chars().count() > MAX_DISPLAY_PATH_LEN {
        out = out.chars().take(MAX_DISPLAY_PATH_LEN).collect();
        out.push('…');
    }
    out
}

fn source_label(source: Source) -> &'static str {
    match source {
        Source::EnvOverride => "env override",
        Source::Path => "PATH",
        Source::FallbackDir => "install dir",
    }
}

/// One remedy line per [`BrokenReason`], defined exactly once.
fn health_detail(health: &Health) -> &'static str {
    match health {
        Health::Working { .. } => "healthy, but a higher-precedence candidate was selected",
        Health::Broken(BrokenReason::Stub) => {
            "a stub, not the real binary — npm's postinstall never materialized \
             the native binary; reinstall to repair"
        }
        Health::Broken(BrokenReason::NotExecutable) => {
            "not executable — the file is present but its execute bit is unset"
        }
        Health::Broken(BrokenReason::ProbeFailed) => {
            "probe failed — `--version` did not exit successfully"
        }
        Health::Broken(BrokenReason::ProbeTimedOut) => {
            "the `--version` probe timed out — the binary started but never answered"
        }
    }
}

/// Render every rejected candidate and why it was rejected.
///
/// One renderer, so a candidate's reason and its remedy cannot drift apart
/// across call sites. Every path is run through [`sanitize_display_path`].
pub fn render_rejections(resolution: &Resolution) -> String {
    if resolution.rejected.is_empty() {
        return "  (no candidate binaries were found on PATH or in amplihack's install directories)"
            .to_string();
    }
    let mut out = String::new();
    for candidate in &resolution.rejected {
        out.push_str(&format!(
            "  - {} [{}]: {}\n",
            sanitize_display_path(&candidate.path),
            source_label(candidate.source),
            health_detail(&candidate.health),
        ));
    }
    out
}

#[cfg(test)]
#[path = "launch_target_tests.rs"]
mod tests;
