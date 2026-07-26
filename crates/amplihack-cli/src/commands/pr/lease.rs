//! PR-ownership lease integration for `amplihack pr watch-and-merge`.
//!
//! Wires the [`amplihack_orchestration::PrLease`] coordination primitive into
//! the merge call site so two concurrent sessions cannot both drive the same PR
//! to merge (issue #1051). The lease is acquired before polling begins and its
//! ownership is asserted immediately before `gh pr merge`; it is released on a
//! successful merge and, as a safety net, when the handle is dropped at the end
//! of the command.

use std::cell::RefCell;
use std::path::PathBuf;
use std::time::Duration;

use amplihack_orchestration::{Clock, FileLeaseStore, LeaseError, LeaseKey, PrLease, SystemClock};
use anyhow::{Context, Result, anyhow};

use super::GhRunner;
use super::{WatchAndMergeArgs, poll_and_merge_gated};

/// Guard invoked immediately before and after a merge so the PR-ownership lease
/// (issue #1051) can veto a merge this session no longer owns and release the
/// lease once the merge lands. The default [`NoopGate`] preserves the ungated
/// behavior for unit tests and callers that manage coordination elsewhere.
pub trait MergeGate {
    /// Called immediately before `gh pr merge`, with no I/O between this check
    /// and the merge. Returning `Err` aborts the merge.
    fn assert_can_merge(&self) -> Result<()>;

    /// Called immediately after a successful merge to release the lease.
    fn on_merged(&self) -> Result<()>;
}

/// A gate that permits every merge and releases nothing. Used by unit tests and
/// the 3-argument `poll_and_merge` wrapper.
pub struct NoopGate;

impl MergeGate for NoopGate {
    fn assert_can_merge(&self) -> Result<()> {
        Ok(())
    }
    fn on_merged(&self) -> Result<()> {
        Ok(())
    }
}

/// Redact all but the first 8 characters of an owner/session id for logging.
pub fn redact_owner(owner: &str) -> String {
    let visible: String = owner.chars().take(8).collect();
    if owner.chars().count() > 8 {
        format!("{visible}…")
    } else {
        visible
    }
}

/// Default lease TTL: a crashed session's lease is reclaimable after 15 minutes.
pub const LEASE_TTL: Duration = Duration::from_secs(900);

/// Outcome of trying to take the PR-ownership lease before merging.
pub enum LeaseAcquisition<'a> {
    /// This session owns the lease and may drive the PR to merge.
    Owner(LeasedMergeGate<'a>),
    /// Another live session owns the lease; this session must stand down.
    StandDown { owner: String },
}

/// The persistent state a lease acquisition borrows from. Held on the stack by
/// the caller so the returned gate can borrow it for the command's lifetime.
pub struct LeaseContext {
    store: FileLeaseStore,
    clock: SystemClock,
    key: LeaseKey,
    session_id: String,
}

impl LeaseContext {
    /// Build a lease context for `repo`/`pr_number`, rooting the file store at
    /// `~/.amplihack/state/pr-leases` (or a temp dir if `$HOME` is unset).
    pub fn new(repo: impl Into<String>, pr_number: u64) -> Result<Self> {
        let store = FileLeaseStore::new(lease_dir())
            .context("failed to open the PR-ownership lease directory")?;
        Ok(Self {
            store,
            clock: SystemClock,
            key: LeaseKey::new(repo, pr_number),
            session_id: new_session_id(),
        })
    }

    /// The self-asserted session id used as the lease owner.
    pub fn session_id(&self) -> &str {
        &self.session_id
    }

    /// Attempt to acquire the lease. A live lease held by another session yields
    /// [`LeaseAcquisition::StandDown`] rather than an error.
    pub fn acquire(&self) -> Result<LeaseAcquisition<'_>> {
        match PrLease::acquire(
            &self.store,
            &self.clock,
            self.key.clone(),
            self.session_id.clone(),
            LEASE_TTL,
        ) {
            Ok(lease) => Ok(LeaseAcquisition::Owner(LeasedMergeGate {
                lease: RefCell::new(lease),
            })),
            Err(LeaseError::AlreadyHeld { owner }) => Ok(LeaseAcquisition::StandDown { owner }),
            Err(other) => Err(anyhow!("failed to acquire the PR-ownership lease: {other}")),
        }
    }
}

/// A [`MergeGate`] backed by a held [`PrLease`]. `assert_can_merge` re-reads the
/// store to confirm live ownership immediately before the merge; `on_merged`
/// releases the lease.
pub struct LeasedMergeGate<'a> {
    lease: RefCell<PrLease<'a, FileLeaseStore, SystemClock>>,
}

impl MergeGate for LeasedMergeGate<'_> {
    fn assert_can_merge(&self) -> Result<()> {
        self.lease
            .borrow()
            .assert_owned()
            .map_err(|e| anyhow!("refusing to merge: PR-ownership lease not held: {e}"))
    }

    fn on_merged(&self) -> Result<()> {
        self.lease
            .borrow_mut()
            .release()
            .map_err(|e| anyhow!("failed to release the PR-ownership lease after merge: {e}"))
    }
}

/// Resolve the current repository as `owner/name` via `gh repo view`.
pub fn detect_repo(runner: &dyn GhRunner) -> Result<String> {
    let output = runner
        .run_gh(&[
            "repo",
            "view",
            "--json",
            "nameWithOwner",
            "-q",
            ".nameWithOwner",
        ])
        .context("failed to determine the current repository via `gh repo view`")?;
    if !output.success {
        return Err(anyhow!(
            "could not determine the current repository: {}",
            output.stderr.trim()
        ));
    }
    let repo = output.stdout.trim().to_string();
    if repo.is_empty() {
        return Err(anyhow!(
            "`gh repo view` returned an empty repository name; run inside a git checkout"
        ));
    }
    Ok(repo)
}

/// Directory holding per-PR lease files.
fn lease_dir() -> PathBuf {
    let base = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(std::env::temp_dir);
    base.join(".amplihack").join("state").join("pr-leases")
}

/// Generate a UUIDv4-grade owner session id.
///
/// Prefers the kernel RNG (`/proc/sys/kernel/random/uuid`) for a real 122-bit
/// random UUID; falls back to a pid + high-resolution timestamp composite when
/// that is unavailable (e.g. non-Linux).
fn new_session_id() -> String {
    if let Ok(uuid) = std::fs::read_to_string("/proc/sys/kernel/random/uuid") {
        let trimmed = uuid.trim();
        if !trimmed.is_empty() {
            return trimmed.to_string();
        }
    }
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    format!("session-{pid:x}-{nanos:x}")
}

/// Current time as an RFC 3339 string for audit logging, read through the
/// injected [`Clock`] rather than `Utc::now()` directly.
pub fn now_rfc3339() -> String {
    SystemClock.now().to_rfc3339()
}

/// Acquire the PR-ownership lease, then poll-and-merge under it.
///
/// Detects the current repository, acquires the `(repo, pr_number)` lease and —
/// if another live session already owns it — stands down (read-only) instead of
/// racing to merge. When this session owns the lease, an `--admin` merge emits
/// an audit line and the merge proceeds gated on continued ownership.
pub fn watch_and_merge_leased(
    runner: &dyn GhRunner,
    args: &WatchAndMergeArgs,
    stderr: &mut dyn std::io::Write,
) -> Result<()> {
    let repo = detect_repo(runner)?;
    let ctx = LeaseContext::new(repo.clone(), u64::from(args.pr_number))?;
    match ctx.acquire()? {
        LeaseAcquisition::StandDown { owner } => {
            let _ = writeln!(
                stderr,
                "🛑 Standing down: PR #{} in {repo} is already being driven by another \
                 session (owner {}). Not force-pushing or merging.",
                args.pr_number,
                redact_owner(&owner),
            );
            Ok(())
        }
        LeaseAcquisition::Owner(gate) => {
            if args.admin {
                // Audit trail for a branch-protection bypass: record the owning
                // session and PR. The lease is never a substitute for
                // GitHub-side controls (issue #1050).
                let _ = writeln!(
                    stderr,
                    "🔐 [audit] admin merge authorized for PR #{} in {repo} by session {} at {}",
                    args.pr_number,
                    redact_owner(ctx.session_id()),
                    now_rfc3339(),
                );
            }
            poll_and_merge_gated(runner, args, stderr, &gate)
        }
    }
}
