//! signal-cli JSON-RPC 2.0 transport: newline-delimited JSON over `tokio` TCP.
//!
//! This module has two layers:
//!
//! - **Pure wire helpers** ([`build_send_request`], [`parse_incoming`]) that do
//!   **no I/O** and are unit-tested in isolation over realistic fixture JSON.
//! - The **`SignalTransport`** client that owns the TCP socket and performs the
//!   `create_group` / `send_group` / `quit_group` / `receive` RPCs.

use std::collections::VecDeque;

use serde_json::Value;

/// Maximum size, in bytes, of a single newline-delimited JSON-RPC frame.
///
/// Fail-safe input bound: a peer that never emits a newline (hostile or broken)
/// must not be able to drive unbounded memory growth. Bytes for a single frame
/// are accumulated only up to this cap; a frame that exceeds it is drained (to
/// resynchronize the stream) and skipped. Signal messages are ~2 KiB, so this
/// generous cap never truncates a legitimate frame.
const MAX_FRAME_BYTES: usize = 256 * 1024;

/// Opaque signal-cli group identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroupId(pub String);

impl GroupId {
    #[must_use]
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<String> for GroupId {
    fn from(s: String) -> Self {
        GroupId(s)
    }
}

/// A parsed inbound envelope, normalized across signal-cli message shapes.
///
/// `parse_incoming` populates this from either a group `dataMessage` (an
/// operator message) or a `syncMessage.sentMessage` (the account's own message
/// synced back from another device). Non-group frames (receipts, typing, direct
/// messages) parse successfully with `group_id == None`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Envelope {
    /// E.164 sender number (`sourceNumber` / `source`), if present.
    pub source: Option<String>,
    /// Sending device id (`sourceDevice`), if present.
    pub source_device: Option<u32>,
    /// Group id if this is a group message, else `None`.
    pub group_id: Option<String>,
    /// Message text body, if any.
    pub body: Option<String>,
    /// `true` when derived from a `syncMessage` (the account's own message).
    pub is_sync: bool,
}

impl Envelope {
    /// Whether this envelope carries a group id.
    #[must_use]
    pub fn is_group(&self) -> bool {
        self.group_id.is_some()
    }
}

/// Errors from the pure wire helpers.
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    /// The input line was not valid JSON.
    #[error("invalid JSON frame: {0}")]
    Json(String),
    /// Group membership could not be verified: the target group was absent from
    /// the daemon's listing, or a member number was missing/blank/non-E.164.
    ///
    /// Security: this variant is **number-free** by construction — its `Display`
    /// is a fixed string, so a rejected (possibly attacker-influenced) member
    /// number never leaks into logs or error surfaces.
    #[error("group membership verification failed")]
    Membership,
}

/// Build a JSON-RPC 2.0 `send` request frame for an outbound group message.
///
/// Returns the request object (the transport assigns the `id` and appends the
/// trailing newline when writing to the socket). Shape:
///
/// ```json
/// {"jsonrpc":"2.0","method":"send","params":{"groupId":"...","message":"..."}}
/// ```
#[must_use]
pub fn build_send_request(group_id: &str, body: &str) -> Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "method": "send",
        "params": {
            "groupId": group_id,
            "message": body,
        }
    })
}

/// Parse one newline-delimited JSON-RPC line into a normalized [`Envelope`].
///
/// Tolerant / fail-safe: any structurally-valid JSON parses to an `Envelope`
/// (with best-effort field extraction); only non-JSON input returns
/// [`WireError::Json`]. Handles both `dataMessage.groupInfo.groupId` and the
/// `syncMessage.sentMessage` group shape, and accepts the frame either wrapped
/// as `{"method":"receive","params":{"envelope":{...}}}` or as a bare envelope.
pub fn parse_incoming(line: &str) -> Result<Envelope, WireError> {
    let root: Value = serde_json::from_str(line).map_err(|e| WireError::Json(e.to_string()))?;
    Ok(parse_envelope(&root))
}

/// Extract a normalized [`Envelope`] from an already-parsed JSON-RPC value.
///
/// Shared by [`parse_incoming`] (string entry point) and the transport's
/// `request()` loop, which must recover an inbound notification it happens to
/// read while awaiting an id-response (FIX 1) without re-parsing the raw line.
fn parse_envelope(root: &Value) -> Envelope {
    // Unwrap `{"params":{"envelope":{...}}}` if present, else treat the value
    // itself as the envelope.
    let env = root
        .get("params")
        .and_then(|p| p.get("envelope"))
        .unwrap_or(root);

    let source = env
        .get("source")
        .and_then(Value::as_str)
        .or_else(|| env.get("sourceNumber").and_then(Value::as_str))
        .map(str::to_string);
    let source_device = env
        .get("sourceDevice")
        .and_then(Value::as_u64)
        .map(|n| n as u32);

    let group_id_of = |msg: &Value| -> Option<String> {
        msg.get("groupInfo")
            .filter(|g| !g.is_null())
            .and_then(|g| g.get("groupId"))
            .and_then(Value::as_str)
            .map(str::to_string)
    };
    let body_of = |msg: &Value| -> Option<String> {
        msg.get("message")
            .and_then(Value::as_str)
            .map(str::to_string)
    };

    let (group_id, body, is_sync) =
        if let Some(dm) = env.get("dataMessage").filter(|d| !d.is_null()) {
            (group_id_of(dm), body_of(dm), false)
        } else if let Some(sm) = env.get("syncMessage").filter(|d| !d.is_null()) {
            match sm.get("sentMessage").filter(|d| !d.is_null()) {
                Some(sent) => (group_id_of(sent), body_of(sent), true),
                None => (None, None, true),
            }
        } else {
            (None, None, false)
        };

    Envelope {
        source,
        source_device,
        group_id,
        body,
        is_sync,
    }
}

impl Envelope {
    /// Whether this envelope carries any meaningful inbound content (used to
    /// decide whether an interleaved notification is worth queueing).
    fn is_meaningful(&self) -> bool {
        self.source.is_some() || self.group_id.is_some() || self.body.is_some()
    }
}

/// Parse a signal-cli `listGroups` result and return the target group's E.164
/// membership, validating **every** member number against the crate's single
/// [`validate_e164`](crate::config::resolver::validate_e164) predicate.
///
/// **Fail-closed** ([`WireError::Membership`]) on any of:
/// * the result is not the expected array shape,
/// * the target `group_id` is absent from the listing (we cannot verify a group
///   we were not told about — treating that as "no members" would be unsafe),
/// * the target group has no `members` array,
/// * any member lacks a `number`, or its number is empty / not E.164
///   (`+` then 1..=15 ASCII digits).
///
/// Security: the error carries **no phone number** (see [`WireError::Membership`]),
/// so a rejected member value never leaks into logs.
pub fn parse_group_members(list_result: &Value, group_id: &str) -> Result<Vec<String>, WireError> {
    let groups = list_result.as_array().ok_or(WireError::Membership)?;
    let group = groups
        .iter()
        .find(|g| g.get("id").and_then(Value::as_str) == Some(group_id))
        .ok_or(WireError::Membership)?;
    let members = group
        .get("members")
        .and_then(Value::as_array)
        .ok_or(WireError::Membership)?;

    let mut out = Vec::with_capacity(members.len());
    for member in members {
        let number = member
            .get("number")
            .and_then(Value::as_str)
            .ok_or(WireError::Membership)?;
        // Reuse the single source-of-truth E.164 validator; fail closed (and
        // number-free) on the first non-conforming member.
        crate::config::resolver::validate_e164(number).map_err(|_| WireError::Membership)?;
        out.push(number.to_string());
    }
    Ok(out)
}

/// Newline-delimited JSON-RPC 2.0 client over a `tokio` TCP connection.
///
/// Owns the socket; all methods perform network I/O. The pure helpers above
/// are used internally and are what the unit tests exercise.
pub struct SignalTransport {
    reader: tokio::io::BufReader<tokio::net::tcp::OwnedReadHalf>,
    writer: tokio::net::tcp::OwnedWriteHalf,
    next_id: u64,
    /// Reusable line buffer for `read_line`, so the receive hot loop does not
    /// heap-allocate a fresh `String` for every inbound frame.
    line_buf: String,
    /// Reusable raw-byte accumulator for one frame, bounded by
    /// [`MAX_FRAME_BYTES`]; decoded once into `line_buf` per frame.
    ///
    /// **Cancel-safety (FIX 1):** this persists the *in-progress* frame across
    /// calls. If a `read_line` future is dropped mid-frame (a competing
    /// `select!` arm wins), the bytes it already consumed from the socket live
    /// here and a later `read_line` resumes from them. It is cleared only at a
    /// frame boundary (a complete frame or an oversized drain), never on entry.
    raw_buf: Vec<u8>,
    /// Whether the in-progress frame in `raw_buf` has already exceeded
    /// [`MAX_FRAME_BYTES`]. Persisted alongside `raw_buf` so an oversized frame
    /// split across a dropped future is still drained (not partially decoded).
    frame_oversized: bool,
    /// Inbound notifications parsed while `request()` awaited an id-response.
    ///
    /// **Cancel-safety (FIX 1):** a `receive()` future may be cancelled with a
    /// partial frame buffered in `raw_buf`, after which a `request()` completes
    /// that frame and reads it as an interleaved notification. Rather than
    /// discard it, `request()` queues it here and the next `receive()` drains it
    /// first, so no inbound envelope is ever lost or duplicated.
    pending_incoming: VecDeque<Envelope>,
}

impl SignalTransport {
    /// Connect to the signal-cli JSON-RPC daemon at `endpoint` (`host:port`).
    pub async fn connect(endpoint: &str) -> std::io::Result<Self> {
        use tokio::io::BufReader;
        use tokio::net::TcpStream;

        let stream = TcpStream::connect(endpoint).await?;
        let (read_half, write_half) = stream.into_split();
        Ok(Self {
            reader: BufReader::new(read_half),
            writer: write_half,
            next_id: 1,
            line_buf: String::new(),
            raw_buf: Vec::new(),
            frame_oversized: false,
            pending_incoming: VecDeque::new(),
        })
    }

    /// Connect to the signal-cli JSON-RPC daemon with **bounded retry**.
    ///
    /// External-service resilience: the signal-cli daemon is a separate process
    /// and may not be accepting connections the instant amplihack starts (a
    /// startup race), or may briefly refuse while restarting. Rather than let a
    /// single transient `connect` failure silently disable the whole channel,
    /// this retries [`connect`](Self::connect) up to `max_attempts` times using
    /// **capped exponential backoff**: the delay starts at `base_delay`, doubles
    /// after each failed attempt, and is clamped to `max_delay`.
    ///
    /// The first successful connection short-circuits and is returned
    /// immediately. If every attempt fails, the **last** underlying I/O error is
    /// returned (so the caller sees the real cause, e.g. `ConnectionRefused`).
    /// `max_attempts` is treated as at least `1`, so at least one connect is
    /// always attempted and no backoff sleep occurs on the final attempt.
    ///
    /// This is additive: [`connect`](Self::connect) keeps its exact
    /// single-attempt semantics for callers that want fail-fast behavior.
    pub async fn connect_with_retry(
        endpoint: &str,
        max_attempts: u32,
        base_delay: std::time::Duration,
        max_delay: std::time::Duration,
    ) -> std::io::Result<Self> {
        let attempts = max_attempts.max(1);
        let mut delay = base_delay;
        let mut last_err: Option<std::io::Error> = None;

        for attempt in 1..=attempts {
            match Self::connect(endpoint).await {
                Ok(transport) => return Ok(transport),
                Err(e) => {
                    // Only sleep/back off when another attempt remains; never
                    // pause after the final attempt.
                    if attempt < attempts {
                        tracing::warn!(
                            attempt,
                            max_attempts = attempts,
                            error = %e,
                            "signal transport connect failed; retrying after backoff"
                        );
                        tokio::time::sleep(delay).await;
                        delay = delay.saturating_mul(2).min(max_delay);
                    }
                    last_err = Some(e);
                }
            }
        }

        // `attempts >= 1` guarantees at least one failed attempt populated
        // `last_err` before we reach here; the fallback is defensive only.
        Err(last_err.unwrap_or_else(|| {
            std::io::Error::other("connect_with_retry: no connection attempts were made")
        }))
    }

    /// Write one JSON-RPC request frame (newline-terminated) to the socket.
    async fn write_frame(&mut self, frame: &Value) -> std::io::Result<()> {
        use tokio::io::AsyncWriteExt;
        let mut line = serde_json::to_string(frame)
            .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
        line.push('\n');
        self.writer.write_all(line.as_bytes()).await?;
        self.writer.flush().await
    }

    /// Read one newline-delimited line from the socket (`None` on EOF).
    ///
    /// Reads into a reusable internal buffer and returns a borrow of it, so the
    /// receive loop avoids a per-frame allocation. The returned slice is valid
    /// until the next `read_line` call.
    ///
    /// The read is **bounded** by [`MAX_FRAME_BYTES`]: a frame larger than the
    /// cap is drained to the next newline (to resynchronize the stream) and
    /// reported as an empty line, which callers skip. This prevents a peer that
    /// never sends a newline from driving unbounded memory growth.
    ///
    /// **Cancel-safe (FIX 1):** this does **not** clear `raw_buf`/
    /// `frame_oversized` on entry. Each `fill_buf().await` is a cancellation
    /// point; if the future is dropped there, the bytes already appended to
    /// `raw_buf` (and consumed from the `BufReader`) would be lost forever if a
    /// later call reset the buffer. Instead the in-progress frame persists and
    /// the buffer is reset only *after* a complete frame or an oversized drain
    /// is produced below — so a resumed read continues from where it left off.
    async fn read_line(&mut self) -> std::io::Result<Option<&str>> {
        use tokio::io::AsyncBufReadExt;

        // If a prior (possibly cancelled) call already buffered part of a frame,
        // we are resuming it — that counts as having read bytes.
        let mut read_any = !self.raw_buf.is_empty();
        loop {
            let available = self.reader.fill_buf().await?;
            if available.is_empty() {
                break; // EOF
            }
            read_any = true;

            let (consumed, done) = match available.iter().position(|&b| b == b'\n') {
                Some(pos) => {
                    let end = pos + 1;
                    if !self.frame_oversized && self.raw_buf.len() + end <= MAX_FRAME_BYTES {
                        self.raw_buf.extend_from_slice(&available[..end]);
                    } else {
                        self.frame_oversized = true;
                    }
                    (end, true)
                }
                None => {
                    let len = available.len();
                    if !self.frame_oversized && self.raw_buf.len() + len <= MAX_FRAME_BYTES {
                        self.raw_buf.extend_from_slice(available);
                    } else {
                        self.frame_oversized = true;
                    }
                    (len, false)
                }
            };
            self.reader.consume(consumed);
            if done {
                break;
            }
        }

        if !read_any {
            return Ok(None); // EOF with nothing buffered
        }
        // EOF reached mid-frame with no terminating newline: fall through and
        // decode whatever we have (matching the prior best-effort behavior).

        if self.frame_oversized {
            // Frame exceeded the cap; the stream has been drained to the next
            // newline. Reset the frame boundary and report an empty line so
            // callers skip it (fail-safe).
            self.raw_buf.clear();
            self.frame_oversized = false;
            return Ok(Some(""));
        }
        // Lossy UTF-8 decode directly into `line_buf`, avoiding the
        // intermediate owned String that `String::from_utf8_lossy` allocates on
        // the invalid-byte path. Semantics are identical: one U+FFFD per
        // maximal invalid subsequence (this is exactly how `from_utf8_lossy` is
        // implemented internally).
        self.line_buf.clear();
        for chunk in self.raw_buf.utf8_chunks() {
            self.line_buf.push_str(chunk.valid());
            if !chunk.invalid().is_empty() {
                self.line_buf.push('\u{FFFD}');
            }
        }
        // Frame boundary: the completed frame has been decoded, so reset the
        // raw accumulator for the next one.
        self.raw_buf.clear();
        Ok(Some(self.line_buf.as_str()))
    }

    /// Send a request and read frames until the matching `id` response arrives,
    /// returning its `result` value. Interleaved `receive` notifications are
    /// skipped.
    async fn request(&mut self, method: &str, params: Value) -> std::io::Result<Value> {
        let id = self.next_id;
        self.next_id += 1;
        let frame = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });
        self.write_frame(&frame).await?;

        loop {
            let Some(line) = self.read_line().await? else {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::UnexpectedEof,
                    "connection closed before response",
                ));
            };
            let Ok(value) = serde_json::from_str::<Value>(line.trim()) else {
                continue;
            };
            // `value` is owned, so the `line` borrow of `self` has ended and we
            // may mutate `self.pending_incoming` below.
            if value.get("id").and_then(Value::as_u64) == Some(id) {
                if let Some(err) = value.get("error").filter(|e| !e.is_null()) {
                    return Err(std::io::Error::other(format!("JSON-RPC error: {err}")));
                }
                return Ok(value.get("result").cloned().unwrap_or(Value::Null));
            }
            // Not our id-response. If it is an inbound notification (e.g. a
            // `receive` frame that arrived while we awaited this response),
            // queue it so a concurrent/cancelled `receive()` does not lose it
            // (FIX 1). Non-envelope frames (stray responses) decode to an empty
            // envelope and are dropped.
            let env = parse_envelope(&value);
            if env.is_meaningful() {
                self.pending_incoming.push_back(env);
            }
        }
    }

    /// Create a group by name (wraps the signal-cli `updateGroup` create-by-name
    /// RPC) and return its [`GroupId`].
    pub async fn create_group(&mut self, name: &str) -> std::io::Result<GroupId> {
        let result = self
            .request("updateGroup", serde_json::json!({ "name": name }))
            .await?;
        // signal-cli returns the new/updated group id under `groupId`.
        let gid = result
            .get("groupId")
            .and_then(Value::as_str)
            .or_else(|| {
                result
                    .get("results")
                    .and_then(|r| r.get("groupId"))
                    .and_then(Value::as_str)
            })
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidData,
                    "updateGroup response missing groupId",
                )
            })?;
        Ok(GroupId(gid.to_string()))
    }

    /// Post `body` to `group_id` (wraps the `send` RPC).
    pub async fn send_group(&mut self, group_id: &GroupId, body: &str) -> std::io::Result<()> {
        let params = build_send_request(group_id.as_str(), body)
            .get("params")
            .cloned()
            .unwrap_or(Value::Null);
        self.request("send", params).await.map(|_| ())
    }

    /// Leave / close a group (`quitGroup`).
    pub async fn quit_group(&mut self, group_id: &GroupId) -> std::io::Result<()> {
        self.request(
            "quitGroup",
            serde_json::json!({ "groupId": group_id.as_str() }),
        )
        .await
        .map(|_| ())
    }

    /// Fetch the live E.164 membership of `group_id` from the daemon.
    ///
    /// Wraps the signal-cli `listGroups` RPC and delegates to the fail-closed
    /// [`parse_group_members`]. Used for per-post membership re-verification
    /// (FIX 3): the answer is never cached, so each call reflects the group's
    /// membership *now*. A membership that cannot be verified (absent group,
    /// malformed member) surfaces as an I/O error whose message is number-free.
    pub async fn group_members(&mut self, group_id: &GroupId) -> std::io::Result<Vec<String>> {
        let result = self.request("listGroups", serde_json::json!({})).await?;
        parse_group_members(&result, group_id.as_str())
            .map_err(|e| std::io::Error::other(e.to_string()))
    }

    /// Read and parse the next inbound envelope from the receive stream.
    ///
    /// Returns `Ok(None)` at end-of-stream. Lines that are not valid JSON are
    /// skipped (fail-safe) rather than aborting the stream.
    pub async fn receive(&mut self) -> std::io::Result<Option<Envelope>> {
        // Drain any notification queued by `request()` while it awaited an
        // id-response (FIX 1) before reading new bytes off the socket.
        if let Some(env) = self.pending_incoming.pop_front() {
            return Ok(Some(env));
        }
        loop {
            let Some(line) = self.read_line().await? else {
                return Ok(None);
            };
            let trimmed = line.trim();
            if trimmed.is_empty() {
                continue;
            }
            match parse_incoming(trimmed) {
                Ok(env) => return Ok(Some(env)),
                Err(_) => continue,
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_send_request_is_jsonrpc_send() {
        let frame = build_send_request("grp-abc123==", "hello world");
        assert_eq!(frame["jsonrpc"], "2.0");
        assert_eq!(frame["method"], "send");
        assert_eq!(frame["params"]["groupId"], "grp-abc123==");
        assert_eq!(frame["params"]["message"], "hello world");
    }

    #[test]
    fn parse_incoming_rejects_non_json() {
        let err = parse_incoming("<not json>").unwrap_err();
        assert!(matches!(err, WireError::Json(_)));
    }

    #[test]
    fn parse_incoming_data_message_group() {
        let line = r#"{"jsonrpc":"2.0","method":"receive","params":{"envelope":{
            "source":"+15551230001","sourceNumber":"+15551230001","sourceDevice":1,
            "dataMessage":{"message":"do the thing","groupInfo":{"groupId":"grp-abc123=="}}
        },"account":"+15551230000"}}"#;
        let env = parse_incoming(line).expect("parses");
        assert_eq!(env.source.as_deref(), Some("+15551230001"));
        assert_eq!(env.source_device, Some(1));
        assert_eq!(env.group_id.as_deref(), Some("grp-abc123=="));
        assert_eq!(env.body.as_deref(), Some("do the thing"));
        assert!(!env.is_sync);
        assert!(env.is_group());
    }

    #[test]
    fn parse_incoming_sync_message_group_marks_is_sync() {
        let line = r#"{"jsonrpc":"2.0","method":"receive","params":{"envelope":{
            "source":"+15551230000","sourceDevice":1,
            "syncMessage":{"sentMessage":{"message":"session started",
                "groupInfo":{"groupId":"grp-abc123=="}}}
        },"account":"+15551230000"}}"#;
        let env = parse_incoming(line).expect("parses");
        assert_eq!(env.group_id.as_deref(), Some("grp-abc123=="));
        assert_eq!(env.body.as_deref(), Some("session started"));
        assert!(env.is_sync, "syncMessage must set is_sync=true");
    }

    #[test]
    fn parse_incoming_direct_message_has_no_group() {
        let line = r#"{"jsonrpc":"2.0","method":"receive","params":{"envelope":{
            "source":"+15551230001","sourceDevice":1,
            "dataMessage":{"message":"hi","groupInfo":null}
        },"account":"+15551230000"}}"#;
        let env = parse_incoming(line).expect("parses");
        assert_eq!(env.group_id, None);
        assert!(!env.is_group());
    }

    #[test]
    fn parse_incoming_receipt_has_no_group_no_body() {
        let line = r#"{"jsonrpc":"2.0","method":"receive","params":{"envelope":{
            "source":"+15551230001","sourceDevice":1,
            "receiptMessage":{"when":123,"isDelivery":true,"timestamps":[1]}
        },"account":"+15551230000"}}"#;
        let env = parse_incoming(line).expect("parses");
        assert_eq!(env.group_id, None);
        assert_eq!(env.body, None);
    }
}
