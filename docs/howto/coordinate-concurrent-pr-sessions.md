# How to Coordinate Concurrent PR Sessions with the Ownership Lease

This guide shows how to use the PR-ownership lease in
`amplihack-orchestration` so two agent sessions never drive the same GitHub
pull request to merge at the same time. You acquire a lease before force-push,
rebase, or merge; a second session that finds the lease held stands down; and
the lease auto-expires so a crash cannot block the PR forever.

For the full type and error reference, see the
[PR-Ownership Lease Reference](../reference/pr-ownership-lease.md). For the
design rationale, see [PR-Ownership Lease Concepts](../concepts/pr-ownership-lease.md).

## When You Need This

Use the lease whenever a session performs a **PR-mutating action**:

- `git push --force` / rebase onto a PR branch
- `gh pr merge` (any strategy) — including `amplihack pr watch-and-merge`
- Any automated finalization that pushes to or merges the PR

Read-only work (viewing checks, reading diffs) does not need the lease.

## Acquire Before You Act

Acquire the lease keyed on `(repo, pr_number)` before the first mutating
action. Use your session ID as the owner and a TTL that comfortably exceeds
your work window (default 15 minutes).

```rust
use std::time::Duration;
use amplihack_orchestration::{FileLeaseStore, LeaseError, LeaseKey, PrLease, SystemClock};

let store = FileLeaseStore::new("~/.amplihack/state/pr-leases")?;
let clock = SystemClock;
let key = LeaseKey::new("rysweet/amplihack-rs", 1051);

let mut lease = match PrLease::acquire(
    &store,
    &clock,
    key,
    session.session_id(),          // stable, unique owner id
    Duration::from_secs(15 * 60),  // TTL
) {
    Ok(lease) => lease,
    Err(LeaseError::AlreadyHeld { owner }) => {
        eprintln!("PR #1051 is already being driven by session {owner}; standing down.");
        return Ok(());             // do NOT force-push or merge
    }
    Err(e) => return Err(e.into()),
};
```

The key rule: **`AlreadyHeld` means stand down.** Never fall through to a
force-push or merge when another live session owns the PR.

## Gate the Merge

Immediately before the gated action, call `assert_owned()`. Keep zero I/O
between the check and the action so ownership cannot lapse in the gap.

```rust
lease.assert_owned()?;                       // still ours + not expired?
run_gh(&["pr", "merge", "1051", "--squash"])?; // gated action, nothing between
```

If `assert_owned()` returns `NotOwner` or `Expired`, abort the merge. Re-acquire
before retrying, or stand down.

## Release When Done

Release the lease on PR merge, PR close, or session end. `release()` is
idempotent, and `Drop` releases as a safety net if your session exits early.

```rust
// After a successful merge or when the PR is closed:
lease.release()?;

// If you simply drop the handle at session end, the lease is released too:
drop(lease); // Drop calls release() best-effort
```

## Extend a Long Job

If a legitimate job runs longer than the TTL, renew before it expires:

```rust
// Periodically, well before expires_at():
lease.renew()?; // resets the TTL window
```

Renewing keeps ownership without releasing. If `renew()` returns `NotOwner` or
`Expired`, another session already reclaimed the PR — stand down.

## Recover a Crashed Session's Lease

You do not need to do anything special. If the owning session crashes, its
lease expires after the TTL, and the next `acquire()` for the key reclaims it
automatically. No GitHub API call is involved in expiry — it is purely
time-based via the injected clock.

To reclaim sooner in an emergency, delete the stale lease file:

```sh
rm ~/.amplihack/state/pr-leases/rysweet__amplihack-rs__pr-1051.json
```

The next `acquire()` treats the missing file as "no lease held".

## Test Without Real Sleeps

Use `InMemoryLeaseStore` and `MockClock` to exercise TTL behavior instantly:

```rust
use std::time::Duration;
use chrono::{TimeZone, Utc};
use amplihack_orchestration::{InMemoryLeaseStore, LeaseError, LeaseKey, MockClock, PrLease};

let store = InMemoryLeaseStore::default();
let clock = MockClock::new(Utc.timestamp_opt(0, 0).unwrap());
let key = LeaseKey::new("rysweet/amplihack-rs", 1051);
let ttl = Duration::from_secs(900);

// Session A acquires.
let _a = PrLease::acquire(&store, &clock, key.clone(), "session-a", ttl).unwrap();

// Session B is refused while A is live.
let b = PrLease::acquire(&store, &clock, key.clone(), "session-b", ttl);
assert!(matches!(b, Err(LeaseError::AlreadyHeld { .. })));

// Advance past the TTL; B can now reclaim.
clock.advance(Duration::from_secs(901));
let _b = PrLease::acquire(&store, &clock, key, "session-b", ttl).unwrap();
```

## Troubleshooting

**"PR is already being driven by session ..."** — Expected: another live
session holds the lease. Stand down. If that session actually crashed, wait for
the TTL (default 15 min) or delete the stale lease file.

**`assert_owned()` returned `Expired` mid-job** — Your work outran the TTL.
Call `renew()` periodically for long jobs, or acquire with a larger TTL (up to
the 24h clamp).

**`assert_owned()` returned `NotOwner`** — Another session reclaimed the PR
after your TTL lapsed. Do not merge. Re-acquire or stand down.

**Lease file won't reclaim** — Confirm the file is under the configured lease
directory and readable (`0o600`). A corrupt or future-versioned record is
treated as reclaimable on the next `acquire()`.

**Two sessions still both merged** — Check that `assert_owned()` is the last
call before `gh pr merge` with no I/O in between, and that both sessions use the
same lease directory. The lease only coordinates sessions that share a store.

## Related

- [PR-Ownership Lease Reference](../reference/pr-ownership-lease.md) - Types, errors, on-disk format, configuration
- [PR-Ownership Lease Concepts](../concepts/pr-ownership-lease.md) - Design rationale and trade-offs
- [How to Watch CI and Auto-Merge a Pull Request](watch-and-merge-pr.md) - The `amplihack pr watch-and-merge` gated site
