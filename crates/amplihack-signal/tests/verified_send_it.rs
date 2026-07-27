//! Finding 1 (Step 17c security review) regression: the outbound membership
//! re-verification gate (`bridge::verified_send`) must fail closed on **every**
//! send, so a group that gained an unexpected member after session start never
//! receives another post — the TOCTOU defense the FIX-3 work promises.
//!
//! These drive the REAL `verified_send` against the offline `FakeSignalEndpoint`
//! whose `listGroups` membership is scripted to change between calls.
#![cfg(feature = "signal")]

use amplihack_signal::bridge::membership::{Membership, expected_members};
use amplihack_signal::bridge::verified_send;
use amplihack_signal::config::{ENV_ACCOUNT, ENV_ALLOWLIST, ENV_ENDPOINT, SignalConfig};
use amplihack_signal::fake_endpoint::FakeSignalEndpoint;
use amplihack_signal::transport::{GroupId, SignalTransport};
use std::collections::HashMap;

const ACCOUNT: &str = "+15551230000";
const OPERATOR: &str = "+15551230001";
const INTRUDER: &str = "+15559990000";

/// Config where the bot's own `account` is intentionally NOT on the allowlist
/// (the dedicated-number model), so `expected_members` must add it back.
fn config_for(addr: &str) -> SignalConfig {
    let mut env = HashMap::new();
    env.insert(ENV_ENDPOINT.to_string(), addr.to_string());
    env.insert(ENV_ACCOUNT.to_string(), ACCOUNT.to_string());
    env.insert(ENV_ALLOWLIST.to_string(), OPERATOR.to_string());
    SignalConfig::from_sources(&env, None).expect("valid config")
}

#[tokio::test]
async fn verified_send_posts_when_membership_matches_expected() {
    let fake = FakeSignalEndpoint::start()
        .await
        .unwrap()
        .with_group_id("grp-ok==")
        // account + operator == expected: exactly the operator-only set.
        .with_group_members_script(vec![vec![ACCOUNT.to_string(), OPERATOR.to_string()]]);
    let mut transport = SignalTransport::connect(fake.addr()).await.unwrap();
    let cfg = config_for(fake.addr());
    let expected = expected_members(&cfg);

    let membership = verified_send(
        &mut transport,
        &GroupId("grp-ok==".to_string()),
        &expected,
        "assistant turn output",
    )
    .await
    .unwrap();

    assert_eq!(membership, Membership::Verified);
    assert!(
        fake.sent()
            .iter()
            .any(|(g, b)| g == "grp-ok==" && b == "assistant turn output"),
        "a verified operator-only group must receive the post; got {:?}",
        fake.sent()
    );
}

#[tokio::test]
async fn verified_send_withholds_when_an_unexpected_member_joined() {
    let fake = FakeSignalEndpoint::start()
        .await
        .unwrap()
        .with_group_id("grp-intruder==")
        // account + operator + an un-allowlisted intruder: must fail closed.
        .with_group_members_script(vec![vec![
            ACCOUNT.to_string(),
            OPERATOR.to_string(),
            INTRUDER.to_string(),
        ]]);
    let mut transport = SignalTransport::connect(fake.addr()).await.unwrap();
    let cfg = config_for(fake.addr());
    let expected = expected_members(&cfg);

    let membership = verified_send(
        &mut transport,
        &GroupId("grp-intruder==".to_string()),
        &expected,
        "sensitive conversation content",
    )
    .await
    .unwrap();

    assert!(
        matches!(membership, Membership::Unverified(_)),
        "an unexpected member must withhold the relay, got {membership:?}"
    );
    assert!(
        fake.sent().is_empty(),
        "nothing may be sent when membership is unverified; got {:?}",
        fake.sent()
    );
}

#[tokio::test]
async fn verified_send_withholds_when_membership_query_is_ambiguous() {
    // Empty script => `listGroups` reports no known group => fail closed.
    let fake = FakeSignalEndpoint::start()
        .await
        .unwrap()
        .with_group_id("grp-unknown==");
    let mut transport = SignalTransport::connect(fake.addr()).await.unwrap();
    let cfg = config_for(fake.addr());
    let expected = expected_members(&cfg);

    let membership = verified_send(
        &mut transport,
        &GroupId("grp-unknown==".to_string()),
        &expected,
        "must not leak",
    )
    .await
    .unwrap();

    assert!(
        matches!(membership, Membership::Unverified(_)),
        "an ambiguous/failed membership query must withhold, got {membership:?}"
    );
    assert!(
        fake.sent().is_empty(),
        "nothing may be sent when membership cannot be verified; got {:?}",
        fake.sent()
    );
}

#[tokio::test]
async fn expected_members_authorizes_account_not_on_allowlist() {
    // Dedicated-number model: account is a group member but never allowlisted.
    let cfg = config_for("127.0.0.1:65000");
    let expected = expected_members(&cfg);
    assert!(
        expected.contains(&ACCOUNT.to_string()),
        "the bot's own account must be treated as an expected member"
    );
    assert!(
        expected.contains(&OPERATOR.to_string()),
        "allowlisted operators must be expected members"
    );
}
