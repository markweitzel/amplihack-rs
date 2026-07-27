//! PR-ownership lease (issue #1051).
//!
//! A coordination primitive that prevents two agent sessions from concurrently
//! driving the same GitHub pull request to merge. A session must [`PrLease::acquire`]
//! the lease for a `(repo, pr_number)` key before it force-pushes, rebases, or
//! merges that PR. A second session that finds the key already held **stands
//! down** (`LeaseError::AlreadyHeld`). The lease **auto-expires** via a TTL so a
//! crashed session cannot permanently block a PR, and it is **released** on
//! merge, close, or session end (explicit [`PrLease::release`] plus a `Drop`
//! safety net).
//!
//! This is a cooperation mechanism, not a security boundary: `owner_session_id`
//! is self-asserted and the real trust boundary is the filesystem permissions on
//! the lease directory. GitHub-side branch protection / merge queue (issue #1050)
//! remains the authoritative gate for untrusted actors.
//!
//! See `docs/reference/pr-ownership-lease.md` for the full contract.

mod clock;
mod store;

pub use clock::{Clock, MockClock, SystemClock};
pub use store::{FileLeaseStore, InMemoryLeaseStore, LeaseStore};

use std::time::Duration;

use chrono::{DateTime, Utc};
use serde::{Deserialize, Serialize};

/// Current on-disk schema version for [`LeaseRecord`].
pub const SCHEMA_VERSION: u32 = 1;

/// Upper bound on a lease TTL. Values above this are clamped so an absurd
/// `ttl_secs` can never overflow expiry arithmetic into a permanent lease.
pub const MAX_TTL_SECS: u64 = 86_400; // 24h

/// Number of compare-and-set attempts before `acquire` reports contention.
const ACQUIRE_ATTEMPTS: u32 = 5;

// ---------------------------------------------------------------------------
// Errors
// ---------------------------------------------------------------------------

/// Errors from lease operations.
#[non_exhaustive]
#[derive(Debug, thiserror::Error)]
pub enum LeaseError {
    /// A live lease is held by another session. The caller must stand down.
    #[error("PR lease already held by session {owner}")]
    AlreadyHeld { owner: String },

    /// This session does not own the lease for a gated action.
    #[error("this session does not own the PR lease")]
    NotOwner,

    /// The lease TTL lapsed before the action.
    #[error("the PR lease has expired")]
    Expired,

    /// Underlying I/O failure.
    #[error("PR lease I/O error: {0}")]
    Io(#[from] std::io::Error),

    /// Stored record could not be decoded (treated as reclaimable).
    #[error("PR lease record is corrupt")]
    Corrupt,

    /// Stored `schema_version` is newer/unknown (treated as reclaimable).
    #[error("PR lease record has an unsupported schema version")]
    UnsupportedVersion,
}

// ---------------------------------------------------------------------------
// Key
// ---------------------------------------------------------------------------

/// The logical primary key for a lease.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LeaseKey {
    /// `"owner/name"`, e.g. `"rysweet/amplihack-rs"`.
    pub repo: String,
    /// Pull request number.
    pub pr_number: u64,
}

impl LeaseKey {
    /// Construct a key for `repo` and `pr_number`.
    pub fn new(repo: impl Into<String>, pr_number: u64) -> Self {
        Self {
            repo: repo.into(),
            pr_number,
        }
    }

    /// Traversal-safe filename slug for per-key file paths.
    ///
    /// Every character of `repo` that is not `[A-Za-z0-9_-]` (including path
    /// separators and `.`) is replaced with `_`. This is a *lexical* sanitizer:
    /// the lease file does not exist yet, so `fs::canonicalize` cannot be used.
    /// A hostile `repo = "../../etc"` cannot survive into the filename — the
    /// result contains no `/`, `\`, or `..` sequence.
    pub fn file_slug(&self) -> String {
        let mut sanitized = String::with_capacity(self.repo.len());
        for c in self.repo.chars() {
            if c.is_ascii_alphanumeric() || c == '-' || c == '_' {
                sanitized.push(c);
            } else {
                sanitized.push('_');
            }
        }
        format!("{sanitized}__pr-{}.json", self.pr_number)
    }
}

// ---------------------------------------------------------------------------
// Record + on-disk container
// ---------------------------------------------------------------------------

/// The persisted lease state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LeaseRecord {
    /// On-disk schema version (currently [`SCHEMA_VERSION`]).
    pub schema_version: u32,
    /// The `(repo, pr_number)` this record locks.
    pub key: LeaseKey,
    /// Self-asserted owning session id.
    pub owner_session_id: String,
    /// When the lease was acquired (RFC 3339 on disk).
    pub acquired_at: DateTime<Utc>,
    /// Time-to-live in seconds. Clamped to [`MAX_TTL_SECS`] for expiry.
    pub ttl_secs: u64,
}

impl LeaseRecord {
    /// Instant after which the lease is considered expired.
    ///
    /// Uses checked arithmetic with a clamped TTL, so an overflowing `ttl_secs`
    /// saturates to the maximum representable instant instead of panicking.
    pub fn expires_at(&self) -> DateTime<Utc> {
        let secs = self.ttl_secs.min(MAX_TTL_SECS);
        self.acquired_at
            .checked_add_signed(chrono::Duration::seconds(secs as i64))
            .unwrap_or(DateTime::<Utc>::MAX_UTC)
    }

    /// True when `now >= expires_at()` (expiry is inclusive).
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool {
        now >= self.expires_at()
    }
}

/// On-disk container wrapping `Option<LeaseRecord>`.
///
/// `Default` is `None`, so an absent or empty file reads back as "no lease
/// held". This wrapper satisfies the `AtomicJsonFile::update` bounds
/// (`Serialize + DeserializeOwned + Default + Clone`).
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct LeaseFile {
    #[serde(default)]
    pub record: Option<LeaseRecord>,
}

// ---------------------------------------------------------------------------
// PrLease (RAII handle)
// ---------------------------------------------------------------------------

/// An RAII handle representing this session's ownership of one PR lease.
///
/// The handle borrows the [`LeaseStore`] and [`Clock`] so `assert_owned`,
/// `renew`, and `release` can reach the backing store without re-passing it.
/// Dropping the handle releases the lease as a best-effort safety net.
pub struct PrLease<'a, S: LeaseStore, C: Clock> {
    store: &'a S,
    clock: &'a C,
    key: LeaseKey,
    owner_session_id: String,
    ttl_secs: u64,
    released: bool,
}

impl<'a, S: LeaseStore, C: Clock> PrLease<'a, S, C> {
    /// Acquire the lease for `key` on behalf of `owner_session_id`.
    ///
    /// Succeeds when no record exists, the existing record is expired (reclaim),
    /// or the existing record is already owned by `owner_session_id`
    /// (idempotent). A corrupt or unsupported-version record is treated as
    /// reclaimable — it never permanently blocks a PR.
    ///
    /// Returns `Err(LeaseError::AlreadyHeld { owner })` when a live record is
    /// owned by a *different* session. The caller must stand down.
    pub fn acquire(
        store: &'a S,
        clock: &'a C,
        key: LeaseKey,
        owner_session_id: impl Into<String>,
        ttl: Duration,
    ) -> Result<Self, LeaseError> {
        let owner = owner_session_id.into();
        let ttl_secs = ttl.as_secs().clamp(1, MAX_TTL_SECS);

        for _ in 0..ACQUIRE_ATTEMPTS {
            let now = clock.now();

            let current = match store.load(&key) {
                Ok(current) => current,
                // A corrupt / future-versioned record is reclaimable: overwrite
                // it unconditionally rather than deadlocking the PR forever.
                Err(LeaseError::Corrupt | LeaseError::UnsupportedVersion) => {
                    let record = Self::build_record(&key, &owner, now, ttl_secs);
                    store.store(&record)?;
                    return Ok(Self::handle(store, clock, key, owner, ttl_secs));
                }
                Err(other) => return Err(other),
            };

            // A record owned by another live session blocks acquisition; a
            // same-owner (idempotent) or expired (reclaim) record falls through.
            if let Some(existing) = &current
                && !existing.is_expired(now)
                && existing.owner_session_id != owner
            {
                return Err(LeaseError::AlreadyHeld {
                    owner: existing.owner_session_id.clone(),
                });
            }

            let record = Self::build_record(&key, &owner, now, ttl_secs);
            if store.compare_and_set(&key, current.as_ref(), Some(&record))? {
                return Ok(Self::handle(store, clock, key, owner, ttl_secs));
            }
            // Lost the compare-and-set race; another writer moved first. Retry
            // with a fresh read.
        }

        // Persistent contention across all attempts: report the current owner.
        match store.load(&key)? {
            Some(existing) if !existing.is_expired(clock.now()) => Err(LeaseError::AlreadyHeld {
                owner: existing.owner_session_id,
            }),
            _ => Err(LeaseError::AlreadyHeld {
                owner: "unknown".to_string(),
            }),
        }
    }

    fn build_record(key: &LeaseKey, owner: &str, now: DateTime<Utc>, ttl_secs: u64) -> LeaseRecord {
        LeaseRecord {
            schema_version: SCHEMA_VERSION,
            key: key.clone(),
            owner_session_id: owner.to_string(),
            acquired_at: now,
            ttl_secs,
        }
    }

    fn handle(store: &'a S, clock: &'a C, key: LeaseKey, owner: String, ttl_secs: u64) -> Self {
        Self {
            store,
            clock,
            key,
            owner_session_id: owner,
            ttl_secs,
            released: false,
        }
    }

    /// Return `Ok(())` only if this session still owns a live (non-expired)
    /// lease. Call immediately before a gated action (force-push/rebase/merge)
    /// with no additional I/O in between.
    ///
    /// Returns `LeaseError::NotOwner` if another session owns the key (or it is
    /// gone), and `LeaseError::Expired` if the lease TTL lapsed.
    pub fn assert_owned(&self) -> Result<(), LeaseError> {
        match self.store.load(&self.key)? {
            Some(record) if record.owner_session_id == self.owner_session_id => {
                if record.is_expired(self.clock.now()) {
                    Err(LeaseError::Expired)
                } else {
                    Ok(())
                }
            }
            _ => Err(LeaseError::NotOwner),
        }
    }

    /// Extend the lease by resetting `acquired_at` to `clock.now()`.
    ///
    /// Fails with `NotOwner`/`Expired` if ownership was lost before the renew.
    pub fn renew(&mut self) -> Result<(), LeaseError> {
        let now = self.clock.now();
        let current = self.store.load(&self.key)?;
        match &current {
            Some(record) if record.owner_session_id == self.owner_session_id => {
                if record.is_expired(now) {
                    return Err(LeaseError::Expired);
                }
            }
            _ => return Err(LeaseError::NotOwner),
        }

        let renewed = Self::build_record(&self.key, &self.owner_session_id, now, self.ttl_secs);
        if self
            .store
            .compare_and_set(&self.key, current.as_ref(), Some(&renewed))?
        {
            Ok(())
        } else {
            // Someone changed the record between load and swap.
            Err(LeaseError::NotOwner)
        }
    }

    /// Explicitly release the lease (idempotent). Called on PR merge/close and
    /// session end. A no-op if the lease was already released or reclaimed by
    /// another session.
    pub fn release(&mut self) -> Result<(), LeaseError> {
        if self.released {
            return Ok(());
        }
        let current = self.store.load(&self.key)?;
        // Only clear the slot if we still own it; ignore a lost race.
        if let Some(record) = &current
            && record.owner_session_id == self.owner_session_id
        {
            self.store
                .compare_and_set(&self.key, current.as_ref(), None)?;
        }
        self.released = true;
        Ok(())
    }

    /// The `(repo, pr_number)` this lease locks.
    pub fn key(&self) -> &LeaseKey {
        &self.key
    }

    /// The owning session id.
    pub fn owner_session_id(&self) -> &str {
        &self.owner_session_id
    }
}

impl<S: LeaseStore, C: Clock> std::fmt::Debug for PrLease<'_, S, C> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PrLease")
            .field("key", &self.key)
            .field("owner_session_id", &self.owner_session_id)
            .field("ttl_secs", &self.ttl_secs)
            .field("released", &self.released)
            .finish()
    }
}

impl<S: LeaseStore, C: Clock> Drop for PrLease<'_, S, C> {
    /// Best-effort `release()` as a safety net for session end. Errors are
    /// logged, never propagated from `drop`.
    fn drop(&mut self) {
        if self.released {
            return;
        }
        if let Err(err) = self.release() {
            tracing::warn!(
                target: "amplihack_orchestration::pr_lease",
                repo = %self.key.repo,
                pr = self.key.pr_number,
                "failed to release PR lease on drop: {err}"
            );
        }
    }
}
