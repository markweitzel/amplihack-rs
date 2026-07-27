//! FIX 3 — per-post membership re-verification (fail-closed, RED until
//! implemented).
//!
//! Before **every** outbound post, [`SignalSession`] must re-fetch the live
//! group membership (`transport.group_members()`) and re-check it against the
//! allowlist via [`Gate::outbound_members_authorized`]. If the group has gained
//! a member that is not authorized (a TOCTOU membership change between posts),
//! the send is **withheld** — no bytes leave, the call fails closed, and the
//! decision is logged via the existing `WITHHOLDING` tracing/eprintln pattern.
//! There is no caching and no cap: the check runs fresh on each post.
//!
//! This exercises the offline `FakeSignalEndpoint`, extended to answer
//! `listGroups` with a test-settable membership (`set_group_members`).
#![cfg(feature = "signal")]

use std::collections::HashMap;

use amplihack_signal::config::{ENV_ACCOUNT, ENV_ALLOWLIST, ENV_ENDPOINT, SignalConfig};
use amplihack_signal::fake_endpoint::FakeSignalEndpoint;
use amplihack_signal::gating::Gate;
use amplihack_signal::session_channel::{Inbox, SignalSession};
use amplihack_signal::transport::{GroupId, SignalTransport};
use tempfile::TempDir;

fn config_with_allowlist(addr: &str, allowlist: &str) -> SignalConfig {
    let mut env = HashMap::new();
    env.insert(ENV_ENDPOINT.to_string(), addr.to_string());
    env.insert(ENV_ACCOUNT.to_string(), "+15551230000".to_string());
    env.insert(ENV_ALLOWLIST.to_string(), allowlist.to_string());
    SignalConfig::from_sources(&env, None).expect("valid config")
}

#[tokio::test]
async fn post_is_withheld_when_group_gains_an_unauthorized_member() {
    let fake = FakeSignalEndpoint::start()
        .await
        .unwrap()
        .with_group_id("grp-reverify==");
    // Initially the group contains the bot's own account (+15551230000, which
    // signal-cli always reports as a member and is NOT on the allowlist) plus
    // one allowlisted human operator (+15551230001).
    fake.set_group_members(&["+15551230000", "+15551230001"]);

    let transport = SignalTransport::connect(fake.addr()).await.unwrap();
    let dir = TempDir::new().unwrap();
    let inbox = Inbox::new(dir.path().join("inbox.json"), 16);
    let cfg = config_with_allowlist(fake.addr(), "+15551230001");
    let mut session = SignalSession::new(
        transport,
        &cfg,
        GroupId("grp-reverify==".to_string()),
        inbox,
    );

    // 1. Membership is authorized → the post is delivered.
    session
        .post("authorized line")
        .await
        .expect("authorized post must succeed");
    assert!(
        fake.sent()
            .iter()
            .any(|(g, b)| g == "grp-reverify==" && b == "authorized line"),
        "an authorized post must reach the group; got {:?}",
        fake.sent()
    );

    // 2. An un-allowlisted number joins the group between posts.
    fake.set_group_members(&["+15551230000", "+15551230001", "+15559999999"]);

    // 3. The next post must be WITHHELD: fail closed and never hit the wire.
    let result = session.post("secret after breach").await;
    assert!(
        result.is_err(),
        "a post to a group with an unauthorized member must fail closed"
    );
    assert!(
        !fake.sent().iter().any(|(_, b)| b == "secret after breach"),
        "the withheld body must never be delivered; got {:?}",
        fake.sent()
    );
}

#[tokio::test]
async fn announce_reverifies_membership_before_sending() {
    let fake = FakeSignalEndpoint::start()
        .await
        .unwrap()
        .with_group_id("grp-announce==");
    // The group contains the bot account, an allowlisted operator, and an
    // unauthorized member at announce time.
    fake.set_group_members(&["+15551230000", "+15551230001", "+15559999999"]);

    let transport = SignalTransport::connect(fake.addr()).await.unwrap();
    let dir = TempDir::new().unwrap();
    let inbox = Inbox::new(dir.path().join("inbox.json"), 16);
    let cfg = config_with_allowlist(fake.addr(), "+15551230001");
    let mut session = SignalSession::new(
        transport,
        &cfg,
        GroupId("grp-announce==".to_string()),
        inbox,
    );

    let result = session.announce().await;
    assert!(
        result.is_err(),
        "announce must re-verify membership and withhold on an unauthorized member"
    );
    assert!(
        fake.sent().is_empty(),
        "nothing may be announced to an unauthorized group; got {:?}",
        fake.sent()
    );
}

#[test]
fn outbound_authorization_requires_every_non_self_member_allowlisted() {
    // account is +15551230000 (see `config_with_allowlist`); the operators
    // +15551230001 / +15551230002 are the allowlisted humans.
    let cfg = config_with_allowlist("127.0.0.1:7583", "+15551230001,+15551230002");
    let gate = Gate::new(&cfg, "grp-reverify==");

    assert!(
        gate.outbound_members_authorized(&[
            "+15551230000".to_string(),
            "+15551230001".to_string(),
            "+15551230002".to_string(),
        ]),
        "the bot account plus all-allowlisted operators is authorized"
    );
    assert!(
        !gate.outbound_members_authorized(&[
            "+15551230000".to_string(),
            "+15551230001".to_string(),
            "+15559999999".to_string(),
        ]),
        "a single non-allowlisted operator revokes authorization (fail-closed)"
    );
    assert!(
        !gate.outbound_members_authorized(&[]),
        "empty/unknown membership is not authorized (fail-closed)"
    );
}

/// Regression: signal-cli reports the bot's own `account` number as a group
/// member, and in the dedicated-number model operators are not expected to add
/// the bot's number to the allowlist. The self number must be excluded from the
/// allowlist requirement, otherwise every legitimate post fails closed. Any
/// *other* un-allowlisted member still withholds the post.
#[test]
fn outbound_authorization_excludes_bot_account_from_allowlist_requirement() {
    // account +15551230000 is deliberately NOT on the allowlist.
    let cfg = config_with_allowlist("127.0.0.1:7583", "+15551230001");
    let gate = Gate::new(&cfg, "grp-reverify==");

    assert!(
        gate.outbound_members_authorized(&[
            "+15551230000".to_string(), // bot account, not allowlisted
            "+15551230001".to_string(), // allowlisted operator
        ]),
        "the bot's own account must not need to be on the allowlist"
    );
    assert!(
        !gate.outbound_members_authorized(&[
            "+15551230000".to_string(), // bot account
            "+15559999999".to_string(), // intruder: neither self nor allowlisted
        ]),
        "an un-allowlisted non-self member still revokes authorization (fail-closed)"
    );
}
