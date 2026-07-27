//! FIX 1 — cancel-safe inbound receive (regression, RED until implemented).
//!
//! An inbound newline-delimited frame may arrive split across several TCP
//! segments. A competing `tokio::select!` arm can drop the `receive()` future
//! *mid-frame*, and an unrelated `request()` (here `group_members()`) may run
//! before the next `receive()`. None of that may lose or duplicate the inbound
//! envelope:
//!
//! * `read_line()` must **persist** the in-progress frame across a dropped
//!   future instead of clearing its accumulation buffer on entry, so a resumed
//!   read continues from the already-consumed bytes.
//! * `request()` must **not discard** a notification it happens to read while
//!   waiting for its id-response; the parsed [`Envelope`] is queued
//!   (`pending_incoming`) and drained by the next `receive()`.
//!
//! This test uses the real-`TcpListener` chunked-write seam. The server reads
//! the client's intervening `listGroups` request as a synchronization barrier,
//! guaranteeing the first `receive()` was cancelled with only the first segment
//! consumed before the frame is completed on the wire.
//!
//! Wire order is: `[chunk1][chunk2\n][listGroups response\n]`. The notification
//! frame (`chunk1 + chunk2`) is therefore complete *before* the id-response, so
//! `request()` reads it as an interleaved notification and must queue it.
#![cfg(feature = "signal")]

use std::time::Duration;

use amplihack_signal::transport::{GroupId, SignalTransport};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::TcpListener;

const CHUNK1: &str = concat!(
    r#"{"jsonrpc":"2.0","method":"receive","params":{"envelope":{"#,
    r#""source":"+15551230077","sourceDevice":1,"#,
    r#""dataMessage":{"message":"fragmented-once","#
);
const CHUNK2: &str = r#""groupInfo":{"groupId":"grp-frag=="}}}}}"#;

#[tokio::test]
async fn fragmented_inbound_frame_survives_dropped_receive_and_intervening_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap().to_string();

    let server = tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let (read_half, mut write_half) = stream.into_split();
        let mut reader = BufReader::new(read_half);

        // 1. Emit only the first segment of the inbound notification (no newline).
        write_half.write_all(CHUNK1.as_bytes()).await.unwrap();
        write_half.flush().await.unwrap();

        // 2. Barrier: block until the client issues its intervening request.
        //    That only happens after the client's first `receive()` was
        //    cancelled mid-frame, so reaching here proves the cancellation.
        let mut req_line = String::new();
        reader.read_line(&mut req_line).await.unwrap();
        let req: Value = serde_json::from_str(req_line.trim()).unwrap();
        let id = req.get("id").cloned().unwrap_or(Value::Null);
        assert_eq!(
            req.get("method").and_then(Value::as_str),
            Some("listGroups"),
            "intervening request should be the group_members/listGroups call"
        );

        // 3. Complete the notification frame, THEN answer the id-request.
        write_half
            .write_all(format!("{CHUNK2}\n").as_bytes())
            .await
            .unwrap();
        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": [{
                "id": "grp-frag==",
                "name": "session",
                "members": [
                    {"number": "+15551230001"},
                    {"number": "+15551230002"}
                ]
            }]
        });
        let mut resp_line = serde_json::to_string(&resp).unwrap();
        resp_line.push('\n');
        write_half.write_all(resp_line.as_bytes()).await.unwrap();
        write_half.flush().await.unwrap();

        // Keep the socket open so the client's duplicate-check read blocks
        // (rather than hitting EOF) during its sleep window.
        tokio::time::sleep(Duration::from_millis(500)).await;
    });

    let mut transport = SignalTransport::connect(&addr).await.unwrap();

    // First receive is cancelled mid-frame: only CHUNK1 has arrived, so the
    // future can never complete before the timer wins the race.
    tokio::select! {
        _ = transport.receive() => panic!("receive() must not complete on an incomplete frame"),
        _ = tokio::time::sleep(Duration::from_millis(200)) => {}
    }

    // Intervening request runs while a partial inbound frame is buffered.
    let members = transport
        .group_members(&GroupId("grp-frag==".to_string()))
        .await
        .expect("group_members ok");
    assert_eq!(
        members,
        vec!["+15551230001".to_string(), "+15551230002".to_string()],
        "group_members must parse the group's E.164 membership"
    );

    // The fragmented notification must ultimately be delivered — intact.
    let env = transport
        .receive()
        .await
        .expect("receive ok")
        .expect("the fragmented envelope");
    assert_eq!(env.source.as_deref(), Some("+15551230077"));
    assert_eq!(env.group_id.as_deref(), Some("grp-frag=="));
    assert_eq!(
        env.body.as_deref(),
        Some("fragmented-once"),
        "the frame split across TCP segments and a dropped future must arrive intact"
    );

    // ...and exactly once: no duplicate delivery of the same frame.
    tokio::select! {
        r = transport.receive() => panic!("duplicate/unexpected inbound frame: {r:?}"),
        _ = tokio::time::sleep(Duration::from_millis(250)) => {}
    }

    server.await.unwrap();
}
