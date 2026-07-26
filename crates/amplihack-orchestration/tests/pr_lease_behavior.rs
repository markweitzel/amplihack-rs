//! Contract / characterization tests for the PR-ownership lease (issue #1051).
//!
//! GOAL locked by these tests: prevent two agent sessions from concurrently
//! driving the same GitHub PR to merge. A session must **acquire** a lease keyed
//! on `(repo, pr_number)` before it force-pushes, rebases, or merges a PR. A
//! second session that finds the key already held **stands down**. The lease
//! **auto-expires** via a TTL so a crashed session cannot permanently block a
//! PR, and it is **released** on merge, close, or session end.
//!
//! These are TDD tests: they define the contract before the implementation
//! exists and are expected to fail (compile error) until `pr_lease.rs` lands.
//!
//! Behavioral contract (mirrors `docs/reference/pr-ownership-lease.md`):
//! - `acquire` succeeds when the key is unheld, expired, or already owned by the
//!   same session (idempotent).
//! - `acquire` on a live key owned by *another* session returns
//!   `LeaseError::AlreadyHeld { owner }` — the caller stands down.
//! - `assert_owned` gates force-push/rebase/merge: `NotOwner` for a foreign
//!   owner, `Expired` after the TTL lapses.
//! - `renew` pushes `expires_at` forward.
//! - `release` (explicit, on merge/close) and `Drop` (session end) free the key.
//! - Timestamps are `chrono::DateTime<Utc>`; all timing is driven by `MockClock`
//!   so no test calls `std::thread::sleep`.
//! - `LeaseKey::file_slug()` is traversal-safe; corrupt/future records are
//!   reclaimable rather than permanently blocking.

use std::time::Duration;

use chrono::{DateTime, TimeZone, Utc};

use amplihack_orchestration::{
    Clock, FileLeaseStore, InMemoryLeaseStore, LeaseError, LeaseKey, LeaseRecord, LeaseStore,
    MockClock, PrLease, SystemClock,
};

const TTL: Duration = Duration::from_secs(900); // 15 min default

fn epoch() -> DateTime<Utc> {
    Utc.timestamp_opt(0, 0).unwrap()
}

fn key() -> LeaseKey {
    LeaseKey::new("rysweet/amplihack-rs", 1051)
}

// ---------------------------------------------------------------------------
// Acquire / contention / stand-down
// ---------------------------------------------------------------------------

#[test]
fn acquire_succeeds_when_unheld() {
    let store = InMemoryLeaseStore::default();
    let clock = MockClock::new(epoch());

    let lease = PrLease::acquire(&store, &clock, key(), "session-a", TTL)
        .expect("a free key must be acquirable");

    assert_eq!(lease.owner_session_id(), "session-a");
    assert_eq!(lease.key(), &key());
}

#[test]
fn contended_acquire_stands_down() {
    let store = InMemoryLeaseStore::default();
    let clock = MockClock::new(epoch());

    let _a = PrLease::acquire(&store, &clock, key(), "session-a", TTL).unwrap();

    // Session B finds the key live and owned by A -> must stand down.
    let b = PrLease::acquire(&store, &clock, key(), "session-b", TTL);
    match b {
        Err(LeaseError::AlreadyHeld { owner }) => assert_eq!(owner, "session-a"),
        other => panic!("expected AlreadyHeld {{ owner: session-a }}, got {other:?}"),
    }
}

#[test]
fn expired_lease_is_reacquirable() {
    let store = InMemoryLeaseStore::default();
    let clock = MockClock::new(epoch());

    let a = PrLease::acquire(&store, &clock, key(), "session-a", TTL).unwrap();
    drop(a); // release the RAII handle borrow; the record persists in the store
    // Re-insert a *live* record for A directly so expiry (not release) is tested.
    store
        .store(&LeaseRecord {
            schema_version: 1,
            key: key(),
            owner_session_id: "session-a".into(),
            acquired_at: clock.now(),
            ttl_secs: TTL.as_secs(),
        })
        .unwrap();

    // While still within the TTL, B is refused.
    let refused = PrLease::acquire(&store, &clock, key(), "session-b", TTL);
    assert!(matches!(refused, Err(LeaseError::AlreadyHeld { .. })));

    // Advance past the TTL; the crashed-session lease is now reclaimable.
    clock.advance(Duration::from_secs(901));
    let b = PrLease::acquire(&store, &clock, key(), "session-b", TTL)
        .expect("expired lease must be reclaimable by another session");
    assert_eq!(b.owner_session_id(), "session-b");
}

#[test]
fn same_owner_acquire_is_idempotent() {
    let store = InMemoryLeaseStore::default();
    let clock = MockClock::new(epoch());

    let _a1 = PrLease::acquire(&store, &clock, key(), "session-a", TTL).unwrap();
    let a2 = PrLease::acquire(&store, &clock, key(), "session-a", TTL)
        .expect("re-acquiring your own live lease must succeed");
    assert_eq!(a2.owner_session_id(), "session-a");
}

// ---------------------------------------------------------------------------
// Gated-action guard (assert_owned)
// ---------------------------------------------------------------------------

#[test]
fn assert_owned_succeeds_for_live_owner() {
    let store = InMemoryLeaseStore::default();
    let clock = MockClock::new(epoch());

    let lease = PrLease::acquire(&store, &clock, key(), "session-a", TTL).unwrap();
    lease
        .assert_owned()
        .expect("the live owner must pass the gated-action guard");
}

#[test]
fn assert_owned_rejects_non_owner() {
    let store = InMemoryLeaseStore::default();
    let clock = MockClock::new(epoch());

    // A owns the key; B fabricates a handle-like record but never actually owns.
    let lease_a = PrLease::acquire(&store, &clock, key(), "session-a", TTL).unwrap();

    // Simulate B stealing the key underneath A (another session reclaims/writes).
    store
        .store(&LeaseRecord {
            schema_version: 1,
            key: key(),
            owner_session_id: "session-b".into(),
            acquired_at: clock.now(),
            ttl_secs: TTL.as_secs(),
        })
        .unwrap();

    // A's gated-action guard must now refuse: it no longer owns the key.
    match lease_a.assert_owned() {
        Err(LeaseError::NotOwner) => {}
        other => panic!("expected NotOwner after ownership was taken, got {other:?}"),
    }
}

#[test]
fn assert_owned_rejects_after_expiry() {
    let store = InMemoryLeaseStore::default();
    let clock = MockClock::new(epoch());

    let lease = PrLease::acquire(&store, &clock, key(), "session-a", TTL).unwrap();

    // TTL lapses before the gated action.
    clock.advance(Duration::from_secs(901));

    match lease.assert_owned() {
        Err(LeaseError::Expired) => {}
        other => panic!("expected Expired after TTL lapse, got {other:?}"),
    }
}

// ---------------------------------------------------------------------------
// Renew
// ---------------------------------------------------------------------------

#[test]
fn renew_extends_ttl() {
    let store = InMemoryLeaseStore::default();
    let clock = MockClock::new(epoch());

    let mut lease = PrLease::acquire(&store, &clock, key(), "session-a", TTL).unwrap();

    // Advance to just before expiry, then renew to push the window forward.
    clock.advance(Duration::from_secs(800));
    lease.renew().expect("owner may renew a live lease");

    // Advance past what would have been the *original* expiry. Still owned.
    clock.advance(Duration::from_secs(300)); // total 1100s from acquire, 300s from renew
    lease
        .assert_owned()
        .expect("renew must push expires_at forward so the lease is still live");
}

// ---------------------------------------------------------------------------
// Release triggers: merge, close, session end (Drop)
// ---------------------------------------------------------------------------

#[test]
fn release_on_merge_frees_key() {
    let store = InMemoryLeaseStore::default();
    let clock = MockClock::new(epoch());

    let mut lease = PrLease::acquire(&store, &clock, key(), "session-a", TTL).unwrap();
    // Merge path calls release() explicitly.
    lease.release().expect("release must succeed for the owner");

    let b = PrLease::acquire(&store, &clock, key(), "session-b", TTL)
        .expect("after release-on-merge another session may acquire");
    assert_eq!(b.owner_session_id(), "session-b");
}

#[test]
fn release_is_idempotent() {
    let store = InMemoryLeaseStore::default();
    let clock = MockClock::new(epoch());

    let mut lease = PrLease::acquire(&store, &clock, key(), "session-a", TTL).unwrap();
    lease.release().unwrap();
    // Close path may release again; second release is a no-op, not an error.
    lease
        .release()
        .expect("double release (merge then close) must be idempotent");
}

#[test]
fn release_on_close_frees_key() {
    let store = InMemoryLeaseStore::default();
    let clock = MockClock::new(epoch());

    let mut lease = PrLease::acquire(&store, &clock, key(), "session-a", TTL).unwrap();
    // Close path releases the lease.
    lease.release().unwrap();

    let b = PrLease::acquire(&store, &clock, key(), "session-b", TTL)
        .expect("after release-on-close another session may acquire");
    assert_eq!(b.owner_session_id(), "session-b");
}

#[test]
fn drop_releases_on_session_end() {
    let store = InMemoryLeaseStore::default();
    let clock = MockClock::new(epoch());

    {
        let _lease = PrLease::acquire(&store, &clock, key(), "session-a", TTL).unwrap();
        // Session ends here without an explicit release; Drop is the safety net.
    }

    let b = PrLease::acquire(&store, &clock, key(), "session-b", TTL)
        .expect("dropping the lease at session end must free the key");
    assert_eq!(b.owner_session_id(), "session-b");
}

// ---------------------------------------------------------------------------
// Store-level contract: file persistence + compare-and-set race safety
// ---------------------------------------------------------------------------

#[test]
fn file_store_roundtrips() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileLeaseStore::new(dir.path()).expect("FileLeaseStore::new must succeed");

    assert!(
        store.load(&key()).unwrap().is_none(),
        "an unheld key loads as None"
    );

    let record = LeaseRecord {
        schema_version: 1,
        key: key(),
        owner_session_id: "9f2c8b1a-4d3e-4c17-9a2b-7e6f5d4c3b2a".into(),
        acquired_at: epoch(),
        ttl_secs: 900,
    };
    store.store(&record).unwrap();

    let loaded = store
        .load(&key())
        .unwrap()
        .expect("stored record must reload");
    assert_eq!(loaded.owner_session_id, record.owner_session_id);
    assert_eq!(loaded.key, key());
    assert_eq!(loaded.ttl_secs, 900);
    assert_eq!(loaded.acquired_at, epoch());

    store.remove(&key()).unwrap();
    assert!(
        store.load(&key()).unwrap().is_none(),
        "removed key loads as None again"
    );
}

#[test]
fn file_store_works_through_pr_lease() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileLeaseStore::new(dir.path()).unwrap();
    let clock = MockClock::new(epoch());

    let _a = PrLease::acquire(&store, &clock, key(), "session-a", TTL).unwrap();
    let b = PrLease::acquire(&store, &clock, key(), "session-b", TTL);
    assert!(
        matches!(b, Err(LeaseError::AlreadyHeld { .. })),
        "file-backed store must enforce cross-session stand-down"
    );
}

#[test]
fn cas_rejects_concurrent_second_writer() {
    let store = InMemoryLeaseStore::default();

    let a = LeaseRecord {
        schema_version: 1,
        key: key(),
        owner_session_id: "session-a".into(),
        acquired_at: epoch(),
        ttl_secs: 900,
    };
    let b = LeaseRecord {
        schema_version: 1,
        key: key(),
        owner_session_id: "session-b".into(),
        acquired_at: epoch(),
        ttl_secs: 900,
    };

    // First writer swaps None -> A.
    let first = store
        .compare_and_set(&key(), None, Some(&a))
        .expect("cas None->A");
    assert!(first, "first writer wins the empty slot");

    // Second writer that *also* expected None must lose the race.
    let second = store
        .compare_and_set(&key(), None, Some(&b))
        .expect("cas None->B");
    assert!(
        !second,
        "a second writer with a stale `expected` must be rejected"
    );

    // The store still holds A.
    let current = store.load(&key()).unwrap().unwrap();
    assert_eq!(current.owner_session_id, "session-a");
}

// ---------------------------------------------------------------------------
// Security characterization: path-injection + reclaimable corrupt records
// ---------------------------------------------------------------------------

#[test]
fn file_slug_is_traversal_safe() {
    // Path separators are sanitized; the slug never contains a raw separator or
    // a parent-dir component that could escape the lease directory.
    let benign = LeaseKey::new("rysweet/amplihack-rs", 1051).file_slug();
    assert!(!benign.contains('/'));
    assert!(!benign.contains('\\'));
    assert!(!benign.contains(".."));
    assert!(benign.contains("1051"));

    let hostile = LeaseKey::new("../../etc", 7).file_slug();
    assert!(
        !hostile.contains('/') && !hostile.contains('\\') && !hostile.contains(".."),
        "a traversal payload must not survive into the filename: got {hostile:?}"
    );
}

#[test]
fn file_slug_distinguishes_repos_and_prs() {
    let a = LeaseKey::new("rysweet/amplihack-rs", 1051).file_slug();
    let b = LeaseKey::new("rysweet/amplihack-rs", 1050).file_slug();
    let c = LeaseKey::new("other/repo", 1051).file_slug();
    assert_ne!(a, b, "different PR numbers map to different files");
    assert_ne!(a, c, "different repos map to different files");
}

#[test]
fn corrupt_record_is_reclaimable() {
    let dir = tempfile::tempdir().unwrap();
    let store = FileLeaseStore::new(dir.path()).unwrap();
    let clock = MockClock::new(epoch());

    // Write a garbage payload to the lease file for this key.
    let slug = key().file_slug();
    std::fs::write(dir.path().join(&slug), b"{ this is not valid json").unwrap();

    // A corrupt record must never permanently block a PR: the next acquire
    // reclaims it (surfacing Corrupt via load is also acceptable, but acquire
    // itself must ultimately succeed for a well-behaved caller).
    let lease = PrLease::acquire(&store, &clock, key(), "session-a", TTL)
        .expect("a corrupt on-disk record must be reclaimable, never a deadlock");
    assert_eq!(lease.owner_session_id(), "session-a");
}

// ---------------------------------------------------------------------------
// Clock contract
// ---------------------------------------------------------------------------

#[test]
fn mock_clock_advances_and_sets() {
    let clock = MockClock::new(epoch());
    assert_eq!(clock.now(), epoch());

    clock.advance(Duration::from_secs(60));
    assert_eq!(clock.now(), epoch() + chrono::Duration::seconds(60));

    let target = Utc.timestamp_opt(1_000, 0).unwrap();
    clock.set(target);
    assert_eq!(clock.now(), target);
}

#[test]
fn system_clock_is_monotonic_wrt_wall_time() {
    // SystemClock delegates to Utc::now(); two reads are non-decreasing.
    let clock = SystemClock;
    let t1 = clock.now();
    let t2 = clock.now();
    assert!(t2 >= t1, "wall-clock reads must be non-decreasing");
}

// ---------------------------------------------------------------------------
// LeaseRecord expiry arithmetic
// ---------------------------------------------------------------------------

#[test]
fn lease_record_expiry_boundary() {
    let record = LeaseRecord {
        schema_version: 1,
        key: key(),
        owner_session_id: "session-a".into(),
        acquired_at: epoch(),
        ttl_secs: 900,
    };

    let expires = record.expires_at();
    assert_eq!(expires, epoch() + chrono::Duration::seconds(900));

    assert!(!record.is_expired(epoch()));
    assert!(!record.is_expired(epoch() + chrono::Duration::seconds(899)));
    assert!(
        record.is_expired(epoch() + chrono::Duration::seconds(900)),
        "expiry is inclusive: now >= expires_at means expired"
    );
    assert!(record.is_expired(epoch() + chrono::Duration::seconds(1000)));
}

#[test]
fn lease_record_expiry_does_not_overflow() {
    // An absurd ttl must be clamped/checked so expiry never wraps into a
    // permanent lease. `is_expired` must remain well-defined (no panic).
    let record = LeaseRecord {
        schema_version: 1,
        key: key(),
        owner_session_id: "session-a".into(),
        acquired_at: epoch(),
        ttl_secs: u64::MAX,
    };

    // Should not panic; a far-future "now" that is still less than any sane
    // clamp is treated as not-yet-expired, and the call is total.
    let _ = record.is_expired(epoch());
    let _ = record.expires_at();
}
