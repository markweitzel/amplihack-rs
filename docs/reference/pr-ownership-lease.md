---
title: PR-Ownership Lease Reference
last_updated: 2026-07-26
review_schedule: quarterly
owner: orchestration-team
---

# PR-Ownership Lease Reference

The PR-ownership lease is a coordination primitive in `amplihack-orchestration`
that prevents two agent sessions from concurrently driving the same GitHub pull
request to merge. A session must **acquire** the lease for a `(repo, pr_number)`
key before it force-pushes, rebases, or merges that PR. A second session that
finds the key already held **stands down** instead of racing. The lease
**auto-expires** via a TTL so a crashed session cannot permanently block a PR,
and it is **released** on merge, close, or session end.

The lease is a cooperation mechanism, not a security boundary. It coordinates
well-behaved sessions that all honor the same protocol. GitHub-side branch
protection and merge queues (issue #1050) remain the authoritative gate for
untrusted actors.

## Contents

- [Module Location](#module-location)
- [Core Types](#core-types)
- [`PrLease` API](#prlease-api)
- [`LeaseStore` Trait](#leasestore-trait)
- [`Clock` Trait](#clock-trait)
- [`LeaseError`](#leaseerror)
- [On-Disk Format](#on-disk-format)
- [Configuration](#configuration)
- [Behavior Contract](#behavior-contract)
- [Security Model](#security-model)
- [Related](#related)

## Module Location

| Item | Path |
| ---- | ---- |
| Implementation | `crates/amplihack-orchestration/src/pr_lease.rs` |
| Contract tests | `crates/amplihack-orchestration/tests/pr_lease_behavior.rs` |
| Public exports | `crates/amplihack-orchestration/src/lib.rs` |

Timestamps are represented with `chrono::DateTime<Utc>`, so
`crates/amplihack-orchestration/Cargo.toml` adds the workspace `chrono`
dependency:

```toml
chrono = { workspace = true }
```

Public re-exports from the crate root:

```rust
use amplihack_orchestration::{
    Clock, FileLeaseStore, InMemoryLeaseStore, LeaseError, LeaseKey, LeaseRecord,
    LeaseStore, MockClock, PrLease, SystemClock,
};
```

## Core Types

### `LeaseKey`

The logical primary key for a lease.

```rust
pub struct LeaseKey {
    pub repo: String,     // "owner/name", e.g. "rysweet/amplihack-rs"
    pub pr_number: u64,
}

impl LeaseKey {
    pub fn new(repo: impl Into<String>, pr_number: u64) -> Self;

    /// Traversal-safe filename slug for per-key file paths.
    /// Sanitizes `repo` by replacing path separators and rejecting `..`
    /// components (a *lexical* check — the target lease file does not exist
    /// yet, so `fs::canonicalize` cannot be used on it). Rejects keys such as
    /// `repo = "../../etc"`.
    pub fn file_slug(&self) -> String;
}
```

`file_slug()` maps `("rysweet/amplihack-rs", 1051)` to a filename such as
`rysweet__amplihack-rs__pr-1051.json`. Path separators and `..` segments are
replaced or rejected before the slug is used to build a path.

### `LeaseRecord`

The persisted lease state.

```rust
pub struct LeaseRecord {
    pub schema_version: u32,       // currently 1
    pub key: LeaseKey,
    pub owner_session_id: String,
    pub acquired_at: DateTime<Utc>,
    pub ttl_secs: u64,
}

impl LeaseRecord {
    /// Instant after which the lease is considered expired.
    /// Uses checked arithmetic; a `ttl_secs` overflow is clamped, never panics.
    pub fn expires_at(&self) -> DateTime<Utc>;

    /// True when `now >= expires_at()`.
    pub fn is_expired(&self, now: DateTime<Utc>) -> bool;
}
```

Deserialization is panic-free. An unknown `schema_version` or corrupt payload
surfaces as `LeaseError::Corrupt` / `LeaseError::UnsupportedVersion` and is
treated as **reclaimable** — a corrupt record never permanently blocks a PR.

### `LeaseFile`

On-disk container wrapping `Option<LeaseRecord>`. `Default` is `None`, so an
absent or empty file reads back as "no lease held". This wrapper satisfies the
`AtomicJsonFile::update` bounds (`Serialize + DeserializeOwned + Default +
Clone`).

## `PrLease` API

`PrLease` is an RAII handle. While held, it represents this session's ownership
of one PR. Dropping it releases the lease as a safety net.

`acquire` borrows the store and clock, and the returned handle retains access
to them so `assert_owned`, `renew`, and `release` can reach the store without
re-passing it. This makes the handle lifetime-bound in practice — either
`PrLease<'a>` over `&'a S, &'a C`, or an owned `Arc<dyn LeaseStore>` +
`Arc<dyn Clock>`. Choose one before implementation; the bare
`acquire(...) -> Result<PrLease, LeaseError>` signature below elides that
lifetime for readability. Note that `assert_owned` re-reads the store to confirm
current ownership, so it *does* perform I/O — the TOCTOU rule is that no
*additional* I/O sits between that check and the gated action.

```rust
impl PrLease {
    /// Acquire the lease for `key` on behalf of `owner_session_id`.
    ///
    /// Succeeds when:
    /// - no record exists for the key, or
    /// - the existing record is expired (reclaim), or
    /// - the existing record is owned by `owner_session_id` (idempotent).
    ///
    /// Returns `Err(LeaseError::AlreadyHeld { owner })` when a live record is
    /// owned by a *different* session. The caller must stand down.
    pub fn acquire<S, C>(
        store: &S,
        clock: &C,
        key: LeaseKey,
        owner_session_id: impl Into<String>,
        ttl: Duration,
    ) -> Result<PrLease, LeaseError>
    where
        S: LeaseStore,
        C: Clock;

    /// Return `Ok(())` only if this session still owns a live (non-expired)
    /// lease. Call immediately before a gated action with no I/O in between.
    ///
    /// Returns `LeaseError::NotOwner` if another session owns the key, and
    /// `LeaseError::Expired` if the lease TTL lapsed.
    pub fn assert_owned(&self) -> Result<(), LeaseError>;

    /// Extend the lease by resetting `acquired_at` to `clock.now()`.
    /// Fails with `NotOwner`/`Expired` if ownership was lost.
    pub fn renew(&mut self) -> Result<(), LeaseError>;

    /// Explicitly release the lease (idempotent). Called on PR merge/close and
    /// session end. A no-op if the lease was already released or reclaimed.
    pub fn release(&mut self) -> Result<(), LeaseError>;

    pub fn key(&self) -> &LeaseKey;
    pub fn owner_session_id(&self) -> &str;
}

impl Drop for PrLease {
    /// Best-effort `release()` as a safety net for session end / crash-within-
    /// process. Errors are logged, never propagated from `drop`.
    fn drop(&mut self);
}
```

### Ownership check must be TOCTOU-tight

`assert_owned()` must be the **last** step before a gated action. Do not
interleave network calls or other I/O between the check and the action:

```rust
lease.assert_owned()?;          // check ownership
run_gh(&["pr", "merge", ...])?; // gated action — nothing between
```

## `LeaseStore` Trait

Persistence abstraction. `compare_and_set` closes the acquire time-of-check /
time-of-use race by holding the underlying lock across the read-modify-write.

```rust
pub trait LeaseStore {
    fn load(&self, key: &LeaseKey) -> Result<Option<LeaseRecord>, LeaseError>;
    fn store(&self, record: &LeaseRecord) -> Result<(), LeaseError>;
    fn remove(&self, key: &LeaseKey) -> Result<(), LeaseError>;

    /// Atomically replace the record for `key` only if the current value equals
    /// `expected`. Returns `true` on success, `false` if another writer changed
    /// it first. Used by `acquire`, `renew`, and `release`.
    fn compare_and_set(
        &self,
        key: &LeaseKey,
        expected: Option<&LeaseRecord>,
        new: Option<&LeaseRecord>,
    ) -> Result<bool, LeaseError>;
}
```

### Implementations

| Impl | Backing | Use |
| ---- | ------- | --- |
| `InMemoryLeaseStore` | `Mutex<HashMap<..>>` | Contract tests, single-process coordination |
| `FileLeaseStore` | `amplihack_state::AtomicJsonFile` | Real sessions, cross-process coordination |

`FileLeaseStore::new(dir)` stores one `LeaseFile` per key under `dir`. It creates
the directory `0o700` and lease files `0o600`. `compare_and_set` is implemented
over `AtomicJsonFile::update()`, which holds the advisory file lock across the
read-modify-write, so concurrent OS processes cannot both acquire the same key.

> **Implementation note.** `AtomicJsonFile::update` takes an `FnOnce(&mut T)`
> that cannot itself return the compare-and-set outcome. Capture the result via
> a closure side-effect (e.g. a `Cell<bool>` set inside the closure): compare the
> loaded value against `expected`, mutate to `new` only on a match, and record
> whether the swap happened. The advisory lock guarantees the compare and the
> conditional write are one critical section.

```rust
let store = FileLeaseStore::new("~/.amplihack/state/pr-leases")?;
```

## `Clock` Trait

Injected time source. The acquire, expiry, and `assert_owned` paths **never**
call `Utc::now()` directly, so TTL expiry is testable without real sleeps.

```rust
pub trait Clock {
    fn now(&self) -> DateTime<Utc>;
}

pub struct SystemClock;          // production: delegates to Utc::now()

pub struct MockClock { /* .. */ } // tests: manually advanced
impl MockClock {
    pub fn new(start: DateTime<Utc>) -> Self;
    pub fn advance(&self, by: Duration);   // move time forward
    pub fn set(&self, to: DateTime<Utc>);
}
```

## `LeaseError`

```rust
#[non_exhaustive]
pub enum LeaseError {
    /// A live lease is held by another session. Caller must stand down.
    AlreadyHeld { owner: String },
    /// This session does not own the lease for a gated action.
    NotOwner,
    /// The lease TTL lapsed before the action.
    Expired,
    /// Underlying I/O failure.
    Io(std::io::Error),
    /// Stored record could not be decoded (treated as reclaimable).
    Corrupt,
    /// Stored `schema_version` is newer/unknown (treated as reclaimable).
    UnsupportedVersion,
}
```

Caller semantics:

| Error | Meaning | Recommended caller action |
| ----- | ------- | ------------------------- |
| `AlreadyHeld { owner }` | Another live session owns the PR | Stand down; log the owner; do not force-push/merge |
| `NotOwner` | Lost ownership before a gated action | Abort the action; re-acquire or stand down |
| `Expired` | Lease lapsed | Re-acquire before continuing |
| `Corrupt` / `UnsupportedVersion` | Bad on-disk record | Safe to reclaim on next `acquire` |
| `Io` | Filesystem error | Surface; do not silently proceed to merge |

## On-Disk Format

`FileLeaseStore` writes one JSON file per key. Example
`rysweet__amplihack-rs__pr-1051.json`:

```json
{
  "record": {
    "schema_version": 1,
    "key": { "repo": "rysweet/amplihack-rs", "pr_number": 1051 },
    "owner_session_id": "9f2c8b1a-4d3e-4c17-9a2b-7e6f5d4c3b2a",
    "acquired_at": "2026-07-26T18:42:42Z",
    "ttl_secs": 900
  }
}
```

A released or absent lease serializes as `{ "record": null }`.

### Timestamp representation

`acquired_at` is a `chrono::DateTime<Utc>` serialized as an RFC 3339 string
(e.g. `"2026-07-26T18:42:42Z"`). This keeps lease files human-readable and
timezone-stable, which matters because the security model requires leases to be
auditable — an operator inspecting `~/.amplihack/state/pr-leases` can read the
owner and acquisition time directly. The crate depends on the workspace `chrono`
dependency (see [Module Location](#module-location)); `SystemTime`'s opaque
`secs_since_epoch` / `nanos_since_epoch` encoding is intentionally **not** used.

## Configuration

| Setting | Default | Range | Notes |
| ------- | ------- | ----- | ----- |
| TTL (`ttl` arg to `acquire`) | `900s` (15 min) | `>= 1s`, clamped to `<= 86_400s` (24h) | Crashed-session reclaim window |
| Lease directory | `~/.amplihack/state/pr-leases` | any writable dir | `FileLeaseStore::new(dir)` |
| Directory permissions | `0o700` | — | Owner-only |
| File permissions | `0o600` | — | Owner-only |

TTL values above 24h are clamped to 24h. Expiry uses checked addition, so an
absurd `ttl_secs` cannot overflow into a permanent lease.

## Behavior Contract

These invariants are locked by `tests/pr_lease_behavior.rs`:

| Test | Guarantee |
| ---- | --------- |
| `acquire_succeeds_when_unheld` | A free key is acquirable |
| `contended_acquire_stands_down` | Second live session gets `AlreadyHeld` |
| `expired_lease_is_reacquirable` | After TTL (via `MockClock`), another session acquires |
| `same_owner_acquire_is_idempotent` | Re-acquiring your own live lease succeeds |
| `assert_owned_rejects_non_owner` | Gated action by non-owner → `NotOwner` |
| `assert_owned_rejects_after_expiry` | Gated action after TTL → `Expired` |
| `release_on_merge_frees_key` | `release()` lets another session acquire |
| `release_on_close_frees_key` | Same, via the close path |
| `drop_releases_on_session_end` | Dropping `PrLease` frees the key |
| `renew_extends_ttl` | `renew()` pushes `expires_at` forward |
| `file_store_roundtrips` | `FileLeaseStore` persists and reloads a record |
| `cas_rejects_concurrent_second_writer` | `compare_and_set` blocks a lost-update race |

All timing tests use `MockClock`; none call `std::thread::sleep`.

## Security Model

- **Coordination, not authorization.** `owner_session_id` is self-asserted. The
  lease stops cooperating sessions from colliding; it does not stop a malicious
  actor. Authoritative merge gating remains GitHub branch protection / merge
  queue (issue #1050).
- **Filesystem is the trust boundary.** `0o700` dir + `0o600` files keep leases
  readable/writable only by the owning user. The lease directory is rooted at
  `$HOME`; when `$HOME` is unset the CLI refuses to run rather than fall back to
  a predictable, world-shared `/tmp` path (which would expose the directory to
  symlink/DoS attacks on multi-user hosts).
- **Path-injection safe.** `LeaseKey::file_slug()` sanitizes path separators and
  rejects `..` components with a lexical containment check (the lease file does
  not exist yet, so `fs::canonicalize` is not applicable) so `repo = "../../.."`
  cannot escape the lease directory. Guard the final joined path against symlink
  swaps as well.
- **`--admin` merges.** A branch-protection bypass (`gh pr merge --admin`, the
  `--admin` flag on `amplihack pr watch-and-merge`) must be guarded by a
  held+owned lease *and* an audit-log entry recording the owner session and PR.
  The lease is never a substitute for GitHub-side controls (#1050).
- **Panic-free decode.** Corrupt or future-versioned records are reclaimable, so
  a bad file self-heals rather than deadlocking a PR.
- **No shell strings.** All `gh` calls at gated sites use argument vectors
  (`Command::new("gh").arg(...)`), never interpolated shell.
- **Session IDs.** Use a `>= 128-bit` random / UUIDv4-grade `owner_session_id`
  so ownership checks cannot collide by accident.

## Related

- [How to Coordinate Concurrent PR Sessions with the Ownership Lease](../howto/coordinate-concurrent-pr-sessions.md) - Task-oriented usage
- [PR-Ownership Lease Concepts](../concepts/pr-ownership-lease.md) - Why the lease exists and how it is designed
- [How to Watch CI and Auto-Merge a Pull Request](../howto/watch-and-merge-pr.md) - The `amplihack pr watch-and-merge` gated call site
- [Power Steering File Locking](power-steering-file-locking.md) - Related advisory-lock pattern
- Issue #1051 - PR-ownership lease (this feature)
- Issue #1050 - GitHub branch protection / merge queue (out of scope here)
