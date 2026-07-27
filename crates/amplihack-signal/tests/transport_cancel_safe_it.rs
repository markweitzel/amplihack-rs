//! FIX 1 (RED) — cancel-safe inbound receive over a real loopback socket.
//!
//! Contract under test (currently UNIMPLEMENTED — this test is expected to
//! FAIL until FIX 1 lands):
//!
//!   1. `SignalTransport::read_line` must be **cancel-safe**. Its only await
//!      point is `fill_buf().await`; bytes already pulled off the socket for an
//!      in-progress frame must persist in the internal accumulator across
//!      `read_line`/`receive` calls. Dropping a `receive()` future mid-frame
//!      (e.g. losing a `tokio::select!` race) must NOT discard the partial
//!      frame — a later read resumes with the accumulated bytes intact.
//!
//!   2. When `request()` (here driven via `send_group`) reads an interleaved
//!      inbound `receive` notification while waiting for its own response, it
//!      must NOT silently drop that notification. The parsed `Envelope` is
//!      pushed onto `pending_incoming`, and a subsequent `receive()` drains
//!      that queue FIRST — so the notification is delivered exactly once,
//!      never lost, never duplicated.
//!
//! The seam is the existing real-`TcpListener` chunked-write pattern used by
//! the other `transport_*_it.rs` integration tests: the server writes one
//! inbound frame in multiple TCP segments with a gap, during which the client
//! cancels its `receive()` future mid-frame and runs a competing `send_group`
//! request. The fragmented envelope must ultimately arrive whole and unique.
//!
//! With the pre-FIX transport this fails because (a) `read_line` clears its
//! buffers at the top of every call, dropping the first segment on the cancel,
//! and (b) `request()` discards interleaved notifications.
#![cfg(feature = "signal")]

use std::time::Duration;

use amplihack_signal::transport::{GroupId, SignalTransport};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

/// The complete inbound frame that will be split across TCP segments. It is a
/// group `dataMessage` whose body is a sentinel we assert on after reassembly.
fn inbound_frame() -> Vec<u8> {
    let mut f = Vec::new();
    f.extend_from_slice(
        br#"{"jsonrpc":"2.0","method":"receive","params":{"envelope":{"source":"+15551230001","sourceDevice":1,"dataMessage":{"message":"hello from fragments","groupInfo":{"groupId":"grp-frag=="}}}}}"#,
    );
    f.push(b'\n');
    f
}

#[tokio::test]
async fn fragmented_inbound_survives_midframe_cancellation_and_interleaved_request() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();

    let server = tokio::spawn(async move {
        let (mut sock, _) = listener.accept().await.unwrap();

        let frame = inbound_frame();
        let split = frame.len() / 2;

        // Segment 1: the first half of the inbound frame, no trailing newline.
        // The client will consume this into its accumulator, then block on the
        // next `fill_buf().await` — which is exactly where the mid-frame cancel
        // happens.
        sock.write_all(&frame[..split]).await.unwrap();
        sock.flush().await.unwrap();

        // Gap long enough for the client's select timer to fire and drop the
        // in-progress `receive()` future while segment 2 is still outstanding.
        tokio::time::sleep(Duration::from_millis(300)).await;

        // Segment 2: the remainder, completing the frame. A cancel-safe reader
        // resumes with segment 1 still buffered and reassembles the whole line.
        sock.write_all(&frame[split..]).await.unwrap();
        sock.flush().await.unwrap();

        // Now service the interleaved outbound `send` request the client issues
        // after cancelling. Read one JSON-RPC line and reply to its id.
        let mut buf = vec![0u8; 4096];
        let n = sock.read(&mut buf).await.unwrap();
        let req: serde_json::Value =
            serde_json::from_slice(buf[..n].split(|&b| b == b'\n').next().unwrap()).unwrap();
        assert_eq!(req.get("method").and_then(|m| m.as_str()), Some("send"));
        let id = req.get("id").cloned().unwrap();
        let resp = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": { "results": [], "timestamp": 0 }
        });
        let mut line = serde_json::to_string(&resp).unwrap();
        line.push('\n');
        sock.write_all(line.as_bytes()).await.unwrap();
        sock.flush().await.unwrap();

        // Hold the socket briefly so the client can drain, then close so the
        // client's final duplicate-probe read observes a clean EOF.
        tokio::time::sleep(Duration::from_millis(150)).await;
    });

    let mut transport = SignalTransport::connect(&addr.to_string()).await.unwrap();

    // Phase 1: race receive() against a timer that fires while only segment 1
    // has arrived. The timer wins, dropping the receive() future MID-FRAME.
    tokio::select! {
        r = transport.receive() => {
            panic!("receive() must not complete on a partial frame; got {r:?}");
        }
        _ = tokio::time::sleep(Duration::from_millis(100)) => {
            // Cancelled mid-frame: segment 1 is consumed off the socket and must
            // remain buffered for the next read (the cancel-safety property).
        }
    }

    // Phase 2: an intervening outbound request runs while the inbound frame is
    // still incomplete. Its internal read loop will encounter the completed
    // notification (segment 2) before its own response; that notification must
    // be queued, not discarded.
    transport
        .send_group(&GroupId("grp-frag==".to_string()), "interleaved outbound")
        .await
        .expect("interleaved send_group must succeed");

    // Phase 3: the fragmented inbound envelope is delivered intact — reassembled
    // across the cancel boundary and surfaced from the pending queue.
    let env = tokio::time::timeout(Duration::from_secs(2), transport.receive())
        .await
        .expect("receive() must not hang")
        .expect("receive ok")
        .expect("the fragmented envelope must be delivered");
    assert_eq!(env.source.as_deref(), Some("+15551230001"));
    assert_eq!(env.group_id.as_deref(), Some("grp-frag=="));
    assert_eq!(
        env.body.as_deref(),
        Some("hello from fragments"),
        "the frame split across the cancel boundary must reassemble byte-for-byte"
    );

    // Phase 4: it must NOT be duplicated. The next receive sees EOF (server
    // closed) or simply nothing — in no case the same envelope again.
    match tokio::time::timeout(Duration::from_millis(500), transport.receive()).await {
        Ok(Ok(None)) => {} // clean EOF — ideal
        Err(_) => {}       // nothing more to read — also fine
        Ok(Ok(Some(dup))) => panic!("duplicate delivery of the queued envelope: {dup:?}"),
        Ok(Err(e)) => panic!("unexpected receive error on duplicate probe: {e}"),
    }

    server.await.unwrap();
}
