//! FIX 3 (wired) — per-chunk group-membership re-verification of an outbound
//! relay, end-to-end over the offline fake endpoint.
//!
//! [`SignalSession::post_verified`] snapshots the group roster, then re-reads
//! and re-checks it (`group_members()` + `verify_membership()`) immediately
//! before EACH `send_group` chunk. This proves the fail-closed contract:
//!
//!   * a stable roster relays every chunk in order, and
//!   * a member removed mid-body stops the remaining chunks (the relay is
//!     withheld, not silently dropped).
//!
//! The fake's `drop_member_after_send(n)` seam drops the last roster member
//! after the n-th recorded `send`, deterministically simulating a mid-relay
//! membership change without cross-task interleaving.
#![cfg(feature = "signal")]

use std::collections::HashMap;

use amplihack_signal::config::{ENV_ACCOUNT, ENV_ALLOWLIST, ENV_ENDPOINT, SignalConfig};
use amplihack_signal::fake_endpoint::FakeSignalEndpoint;
use amplihack_signal::session_channel::{Inbox, SignalSession};
use amplihack_signal::transport::{GroupId, SignalTransport};
use tempfile::TempDir;

fn config_for(addr: &str) -> SignalConfig {
    let mut env = HashMap::new();
    env.insert(ENV_ENDPOINT.to_string(), addr.to_string());
    env.insert(ENV_ACCOUNT.to_string(), "+15551230000".to_string());
    env.insert(ENV_ALLOWLIST.to_string(), "+15551230000".to_string());
    SignalConfig::from_sources(&env, None).expect("valid config")
}

async fn session_for(fake: &FakeSignalEndpoint, group: &str) -> SignalSession {
    let transport = SignalTransport::connect(fake.addr()).await.unwrap();
    let dir = TempDir::new().unwrap();
    // Leak the TempDir so its path stays valid for the session's lifetime; the
    // OS reclaims it at process exit (tests are short-lived).
    let inbox_path = dir.keep().join("inbox.json");
    let inbox = Inbox::new(inbox_path, 16);
    let cfg = config_for(fake.addr());
    SignalSession::new(transport, &cfg, GroupId(group.to_string()), inbox)
}

#[tokio::test]
async fn stable_roster_relays_all_chunks_in_order() {
    let fake = FakeSignalEndpoint::start()
        .await
        .unwrap()
        .with_group_id("grp-stable==")
        .with_members(&["+15551230000", "+15551230001", "+15551230002"]);
    let mut session = session_for(&fake, "grp-stable==").await;

    session
        .post_verified(&["chunk one", "chunk two", "chunk three"])
        .await
        .expect("stable roster must relay every chunk");

    let bodies: Vec<String> = fake.sent().into_iter().map(|(_, b)| b).collect();
    assert_eq!(
        bodies,
        vec![
            "chunk one".to_string(),
            "chunk two".to_string(),
            "chunk three".to_string()
        ],
        "all chunks relay in order when membership is unchanged"
    );
}

#[tokio::test]
async fn member_removed_midbody_withholds_remaining_chunks() {
    // Drop a roster member right after the first chunk is sent; the re-read
    // before chunk two must observe the drift and withhold the rest.
    let fake = FakeSignalEndpoint::start()
        .await
        .unwrap()
        .with_group_id("grp-tamper==")
        .with_members(&["+15551230000", "+15551230001", "+15551230002"])
        .drop_member_after_send(1);
    let mut session = session_for(&fake, "grp-tamper==").await;

    let result = session
        .post_verified(&["chunk one", "chunk two", "chunk three"])
        .await;

    assert!(
        result.is_err(),
        "a mid-body membership change must fail closed, got {result:?}"
    );

    let bodies: Vec<String> = fake.sent().into_iter().map(|(_, b)| b).collect();
    assert_eq!(
        bodies,
        vec!["chunk one".to_string()],
        "only the pre-tamper chunk is relayed; the rest are withheld, not sent"
    );
    assert_eq!(
        fake.members().len(),
        2,
        "the fake dropped one member mid-relay (sanity check on the tamper seam)"
    );
}

#[tokio::test]
async fn member_altered_midbody_withholds_remaining_chunks() {
    // Removing then relying on set-inequality also covers alteration: swap the
    // roster to a different set before the second chunk by dropping a member.
    let fake = FakeSignalEndpoint::start()
        .await
        .unwrap()
        .with_group_id("grp-alter==")
        .with_members(&["+15551230000", "+15551230001"])
        .drop_member_after_send(1);
    let mut session = session_for(&fake, "grp-alter==").await;

    let result = session
        .post_verified(&["only-first", "should-withhold"])
        .await;
    assert!(result.is_err(), "altered roster must withhold: {result:?}");

    let bodies: Vec<String> = fake.sent().into_iter().map(|(_, b)| b).collect();
    assert_eq!(bodies, vec!["only-first".to_string()]);
}
