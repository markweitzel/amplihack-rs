# PR-Ownership Lease Concepts

This document explains **why** the PR-ownership lease exists, the problem it
solves, and the design decisions behind it. For task-oriented usage see the
[how-to guide](../howto/coordinate-concurrent-pr-sessions.md); for the exact
API see the [reference](../reference/pr-ownership-lease.md).

## The Problem: Concurrent PR Drivers

Amplihack runs multiple agent sessions, sometimes against the same repository.
Two sessions that independently decide to finalize the **same** pull request can
collide:

- Both rebase or force-push the branch, clobbering each other's history.
- Both call `gh pr merge`, producing duplicate merges, race errors, or a merge
  of a stale head.
- One session's force-push invalidates the CI run another session is about to
  merge on.

Nothing in GitHub's default flow stops two well-intentioned automations from
racing. We need a lightweight way for sessions to agree that **exactly one** of
them owns the right to mutate a given PR at a time.

## The Solution: A TTL-Based Cooperative Lease

A lease is a claim on a `(repo, pr_number)` key, held by one session for a
bounded time. The rules are deliberately small:

1. **Acquire before mutating.** A session must hold the lease for a PR before it
   force-pushes, rebases, or merges that PR.
2. **Stand down if contended.** If the key is already held by a live session,
   the second session backs off rather than racing.
3. **Auto-expire.** The lease carries a TTL. If the owner crashes, the lease
   lapses and the PR becomes claimable again — no human unblock required.
4. **Release on completion.** The lease is released when the PR is merged or
   closed, and when the owning session ends.

## Why These Design Choices

### Coordination, not authorization

The lease is a **cooperation** mechanism among sessions that all honor the same
protocol. The `owner_session_id` is self-asserted; a hostile process could
ignore the lease entirely. That is acceptable because the lease is not the
security boundary — GitHub branch protection and the merge queue (issue #1050)
are the authoritative gate for untrusted actors. Keeping the lease's scope to
"stop friendly sessions from colliding" keeps it simple and offline.

### TTL over liveness pings

We reclaim crashed sessions with a time-based TTL rather than by probing whether
the owner is still alive. A liveness check would require network calls, a
heartbeat service, or process introspection across machines — all fragile and
non-deterministic. A TTL needs only a clock: after the window elapses, the lease
is reclaimable. The default is 15 minutes, comfortably longer than a normal
finalize-and-merge cycle, and long jobs call `renew()` to extend it.

### Injected clock

Expiry logic reads time through a `Clock` trait, never `chrono::Utc::now()`
directly. This makes TTL behavior deterministic and unit-testable: tests advance
a `MockClock` to simulate hours passing in microseconds, with no `sleep`. The
production `SystemClock` simply delegates to `Utc::now()`. Timestamps are
`chrono::DateTime<Utc>`, so persisted lease files carry human-readable RFC 3339
acquisition times rather than opaque epoch counters.

### Pluggable store with compare-and-set

Persistence hides behind a `LeaseStore` trait so the same lease logic works in
two settings:

- `InMemoryLeaseStore` for tests and single-process coordination.
- `FileLeaseStore` for real, cross-process sessions, backed by
  `amplihack_state::AtomicJsonFile`.

The critical operation is `compare_and_set`, which performs the acquire's
read-modify-write while holding the underlying lock. Without it, two sessions
could both read "no lease", both write their own, and both believe they won —
the classic time-of-check/time-of-use race. `compare_and_set` collapses that
window so at most one writer succeeds.

### RAII handle with a Drop safety net

`PrLease` is an RAII handle. The happy path releases explicitly on merge/close.
But sessions can exit through many paths — early returns, errors, panics-within-
process. `Drop` calls `release()` best-effort so a lease is not left dangling
when a session ends without an explicit release. The TTL is the final backstop
if even `Drop` cannot run (hard crash, `kill -9`).

## The Ownership Check Is the Sharp Edge

The single most important correctness property is that the ownership check is
**TOCTOU-tight**: `assert_owned()` must be the last thing before the gated
action, with no I/O in between.

```
lease.assert_owned()?;   // A: verify we still own a live lease
gh pr merge ...          // B: gated action
```

If any network or file I/O sits between A and B, ownership could lapse in the
gap and two sessions could both reach B. The API is shaped to make the correct
pattern the obvious one, and the reference documents this rule prominently.

A residual window remains even with zero I/O between A and B: the lease could
expire, or another process could reclaim it, in the instant between the
in-process ownership check and GitHub actually applying the merge. The lease
*shrinks* the double-merge window; it does not eliminate it. That is why the
lease is explicitly coordination, not authorization — GitHub-side gating
(issue #1050) remains the only hard guarantee against a double merge.

## Lifecycle at a Glance

```
        acquire(key, owner, ttl)
              │
        ┌─────▼─────┐   AlreadyHeld{owner}
        │  held by  ├──────────────► second session STANDS DOWN
        │  session  │
        └─────┬─────┘
              │ assert_owned() ── ok ──► force-push / rebase / merge
              │
     ┌────────┼────────────┐
     │        │            │
  merge/    session end   TTL elapsed
  close     (Drop)        (crash)
     │        │            │
     ▼        ▼            ▼
   release  release     auto-expire ──► key reclaimable by next acquire()
```

## What This Is Not

- **Not GitHub branch protection or a merge queue.** Those are issue #1050 and
  require human sign-off; this lease does not change them.
- **Not the engine-side reflection-loop bound.** That is
  `rysweet/amplihack-recipe-runner` issue #132, a separate repository.
- **Not a distributed consensus system.** It coordinates cooperating local
  sessions through a shared store; it does not tolerate Byzantine actors.

## Related

- [PR-Ownership Lease Reference](../reference/pr-ownership-lease.md) - Types, errors, configuration, behavior contract
- [How to Coordinate Concurrent PR Sessions](../howto/coordinate-concurrent-pr-sessions.md) - Practical usage
- [Power Steering File Locking](../reference/power-steering-file-locking.md) - Related advisory-lock coordination pattern
- Issue #1051 - This feature
- Issue #1050 - Branch protection / merge queue (out of scope)
