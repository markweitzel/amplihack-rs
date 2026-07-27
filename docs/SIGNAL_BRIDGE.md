# Signal Bridge

The **Signal bridge** connects a live amplihack session to a private Signal
group over the signal-cli JSON-RPC transport. It mirrors the conversation
outbound (agent → operator) and relays allow-listed operator messages inbound
(operator → agent), under a strict, **fail-closed** trust boundary.

- **Crate (transport + gating):** `amplihack-signal`
- **Host wiring:** `amplihack-cli` (`signal` feature)
- **Cargo feature:** `signal` (default **OFF** — zero cost, no extra deps when off)
- **Wire protocol:** signal-cli JSON-RPC 2.0 over newline-delimited TCP (NDJSON)
- **Dependency direction:** `amplihack-cli` → `amplihack-signal` (never the reverse)

> **Status — implemented (Issue #1064).**
> This page documents the bridge's *hardened* runtime behavior delivered under
> **Issue #1064**: cancel-safe inbound framing, fail-closed E.164
> group-membership validation, and per-post membership re-verification. The
> behavior below is present on the branch when built with `--features signal`.
> For the design rationale and threat model, see
> [Signal Bridge Hardening](signal-bridge-hardening.md). For the onboarding CLI,
> configuration keys, and the end-to-end channel overview, see
> [Signal Channel](signal-channel.md).

---

## Contents

- [Architecture at a glance](#architecture-at-a-glance)
- [Inbound receive: cancel-safe framing](#inbound-receive-cancel-safe-framing)
  - [Persisted partial-frame state](#persisted-partial-frame-state)
  - [Pending-notification drain](#pending-notification-drain)
  - [Frame bounds and resynchronization](#frame-bounds-and-resynchronization)
- [Group membership: E.164 fail-closed](#group-membership-e164-fail-closed)
- [Outbound relay: per-post membership re-verification](#outbound-relay-per-post-membership-re-verification)
- [Trust boundary summary](#trust-boundary-summary)
- [API reference](#api-reference)
- [Building and testing](#building-and-testing)

---

## Architecture at a glance

The base primitives already existed in `amplihack-signal`:
`SignalTransport::{receive, send_group, request}` and `SignalSession::{post,
pump_once}` (which wraps `receive` with the deny-by-default `Gate`). The
hardening added the pieces marked **(hardening)** below.

```
                       ┌──────────────────────────────────────────┐
   operator ── Signal ─┤  signal-cli daemon (JSON-RPC 2.0 / TCP)   │
                       └───────────────▲──────────────┬───────────┘
                                       │ send/receive  │ frames
                       ┌───────────────┴──────────────▼───────────┐
                       │           SignalTransport                 │
                       │  • cancel-safe read_line (persisted buf)  │ (hardening)
                       │  • pending_incoming: VecDeque<Envelope>   │ (hardening)
                       │  • receive() drains queue, then reads     │ (hardening)
                       │  • group_members() (E.164, fail-closed)   │ (hardening)
                       └───────────────▲──────────────┬───────────┘
                                       │ receive()      │ send_group(&GroupId)
                       ┌───────────────┴──────────────▼───────────┐
                       │        SignalSession (amplihack-signal)   │
                       │  • pump_once(): receive() + Gate::evaluate │
                       │  • post_verified(): chunk + re-verify each │ (hardening)
                       └───────────────────────────────────────────┘
```

The bridge's inbound path is intended to be a single `tokio::select!` arm hosted
by the CLI `signal` feature:

```rust
tokio::select! {
    recv = transport.receive() => { /* handle inbound envelope */ }
    // ... other arms: outbound work, shutdown, timers ...
}
```

`receive()` is the **only** inbound reader — there is no separate mpsc channel
and no spawned reader task. Because `select!` may drop the `receive()` future
whenever another arm becomes ready, `receive()` and the framing beneath it must
be **cancellation-safe**: a partially-read frame must never be lost. Making that
guarantee hold is FIX 1.

---

## Inbound receive: cancel-safe framing

### Persisted partial-frame state

**Before the fix.** `SignalTransport::read_line()` cleared its accumulation
buffers (`line_buf`, `raw_buf`) at the top of every call. A `receive()` future
dropped mid-frame therefore lost the bytes it had already read.

**Hardened behavior (FIX 1).** `read_line()` accumulates the bytes of a single
newline-delimited frame into an internal, reusable buffer that **persists across
calls**:

- Bytes are consumed from the socket (`consume`) as they are read, so the same
  bytes are never re-read.
- The accumulation buffer is reset **only after** a complete frame (terminated
  by `\n`) is decoded and returned, or after an oversized frame has been drained
  (see [Frame bounds](#frame-bounds-and-resynchronization)) — never at entry.
- The **sole `.await` / cancellation point** is `fill_buf().await`. Nothing has
  been consumed from the socket at that suspension point, so if the future is
  dropped there, no bytes are lost.

Consequence: if the `receive()` future is dropped mid-frame (e.g. another
`select!` arm fires while only some TCP segments of a frame have arrived), the
already-accumulated bytes remain in the persisted buffer. The **next** call to
`receive()` resumes appending to that same buffer and ultimately delivers the
frame **intact and exactly once** — no truncation, no duplication.

This makes it safe to interleave other transport RPCs (`request()`,
`group_members()`, `send_group()`) between the drop and the resume: those RPCs
read their *own* response frames, while the in-progress inbound frame's bytes
stay parked in the persisted accumulator.

### Pending-notification drain

`SignalTransport::request()` writes a JSON-RPC request and then reads frames
until the frame whose `id` matches the request arrives. Along the way it may
encounter **notification** frames (inbound `receive` envelopes with no matching
`id`). Under FIX 1 these are **not discarded**. Instead, each such frame is
parsed into an `Envelope` and pushed onto a new FIFO queue field added to
`SignalTransport`:

```rust
pending_incoming: VecDeque<Envelope>   // (planned) new field
```

`receive()` **drains `pending_incoming` first**, in arrival order, before
reading any new frame from the socket:

```rust
pub async fn receive(&mut self) -> io::Result<Option<Envelope>> {
    if let Some(env) = self.pending_incoming.pop_front() {
        return Ok(Some(env));
    }
    // ... otherwise read + parse the next frame from the socket ...
}
```

This is **fail-closed against silent loss**: an operator message that happens to
arrive while the bridge is mid-`request()` (e.g. during a `group_members()`
membership check) is queued and delivered on the next `receive()`, never
dropped. Ordering is preserved (FIFO), and each envelope is delivered exactly
once.

### Frame bounds and resynchronization

A single frame is bounded by `MAX_FRAME_BYTES` (**256 KiB**), which already
exists in `transport.rs`. Legitimate Signal frames are ~2 KiB, so the cap never
truncates real traffic; it exists purely as a fail-safe against a hostile or
broken peer that never emits a newline.

- While accumulating, bytes are appended only up to the cap.
- A frame that exceeds the cap is marked **oversized**: its bytes are drained up
  to (and including) the next newline to **resynchronize** the stream, and an
  **empty line** is surfaced to the caller.
- Callers (`receive()`) **skip empty lines** and continue.

This oversized-drain / empty-line resync behavior already exists and must be
**preserved exactly** by the cancel-safe rework; the only change is *where* the
accumulation buffer is reset (after a returned/drained frame, not at entry).

---

## Group membership: E.164 fail-closed

FIX 2 adds a `group_members()` method to `SignalTransport` that reads the
current members of a Signal group and parses the signal-cli membership response
with a new `parse_group_members` helper. Every member number is validated
against the crate's existing E.164 validator:

```rust
// amplihack_signal::config::resolver::validate_e164
// Accepts: '+' followed by 1..=15 ASCII digits.
// Signature: fn validate_e164(s: &str) -> Result<(), ConfigError>
```

Today this validator is `pub(super)` inside the `config` module and returns
`Result<(), ConfigError>` (yielding `ConfigError::InvalidE164` on failure). FIX 2
**promotes its visibility to `pub(crate)`** so the transport can reuse the *same*
validator, and maps its `ConfigError::InvalidE164` failure onto a new
`WireError::Membership` variant (see below). The validator is shared **within**
`amplihack-signal` only; it is **not** imported from `amplihack-cli`, which would
invert the dependency direction and create a cycle.

Validation is **fail-closed on the first offending entry**:

- An **empty** member number → reject the whole parse.
- A **malformed** member number (missing `+`, non-digit characters, zero
  digits, or more than 15 digits) → reject the whole parse.
- On rejection, `parse_group_members` returns a new fail-closed
  `WireError::Membership` error.

`WireError` currently has a single variant, `Json(String)`. FIX 2 adds
`Membership` as a second variant. **No member numbers are leaked** in the error:
the `Membership` message is deliberately generic (it never embeds the offending
number), so audit logs and error surfaces stay PII-free. A new regression test,
`parse_failure_message_does_not_leak_member_numbers`, enforces this.

---

## Outbound relay: per-post membership re-verification

When the bridge relays an operator message (or mirrors a large body) it may split
the body into multiple `send_group` **chunks**. FIX 3 adds
`SignalSession::post_verified(&[&str])` (in `session_channel.rs`, where
`send_group` is actually called). It snapshots the group roster once at the start
of the post, then re-checks membership **immediately before EACH chunk**, not
once per body:

```text
expected = transport.group_members(&group_id)        # snapshot at post start (FIX 2)
for chunk in chunks:
    current = transport.group_members(&group_id)      # fresh re-read (FIX 2)
    if verify_membership(current, expected) == Withhold:
        log WITHHOLDING (tracing::warn! + eprintln!)  # surface — never silent
        return Err(...)   # fail-closed: skip all remaining chunks
    transport.send_group(&group_id, chunk)            # send_group takes &GroupId
    gate.record_outbound(chunk)                       # echo-suppress the sent chunk
```

Membership is re-verified with `transport::verify_membership`, which compares the
re-read roster to the snapshot as **set equality** (order- and
duplicate-insensitive): a benign reorder is `Verified`, but any member removed,
altered, or injected is `Withhold`. This is a deliberate choice — the relay is
authorized against the exact roster present when the post began, so *any* drift
during the body halts it. The existing `post()` (single, unverified `send_group`)
is unchanged; `post_verified()` is the additive fail-closed relay path.

Rationale: a group's membership can change *mid-relay* (a member is
removed/added, or an attacker is injected between chunks). Verifying once
up-front would leave a window in which later chunks are posted to a group whose
membership no longer matches. Re-verifying per chunk closes that window.

On **any** mid-body verification failure:

- Remaining chunks are **not** sent (fail-closed) and the call returns an error.
- The withheld relay is **surfaced**, not silently dropped, using a
  `WITHHOLDING` log line (`tracing::warn!` plus an `eprintln!`), so operators can
  see that — and why — a post was truncated. The log carries no member numbers or
  body text.
- No caps or rate limits are introduced — the behavior is purely
  verify-then-send, chunk by chunk.

---

## Trust boundary summary

The table below states the guarantees delivered by FIX 1–3.

| Property | Behavior |
| --- | --- |
| Inbound framing | Cancel-safe; partial frames persist across dropped `receive()` futures |
| Interleaved notifications | Queued in `pending_incoming` (FIFO), never dropped |
| Frame size | Bounded to 256 KiB; oversized frames drained + skipped (resync) |
| Membership numbers | E.164-validated (`+` then 1..=15 ASCII digits), fail-closed |
| Invalid/empty member | Whole parse rejected via `WireError::Membership` |
| Error messages | Never leak member numbers (PII-free) |
| Outbound relay | Membership re-verified before **each** chunk |
| Mid-body failure | Remaining chunks withheld; `WITHHOLDING` logged (surfaced) |
| Silent loss / silent drop | Never — every withhold and every queued notification is accounted for |

All of the above hold **only** when built with `--features signal`; with the
feature off the bridge is compiled out entirely.

---

## API reference

Selected `amplihack-signal` surface exercised by the bridge (feature `signal`).
Items marked **(hardening)** were introduced by this hardening.

| Item | Kind | Description |
| --- | --- | --- |
| `SignalTransport::connect` / `connect_with_retry` | async fn | Open the JSON-RPC TCP connection (with optional bounded backoff retry). |
| `SignalTransport::receive` | async fn | Return the next inbound `Envelope`. **(hardening)** drains `pending_incoming` first, then reads a new frame; cancel-safe. |
| `SignalTransport::group_members` | async fn | **(hardening)** Read + parse current group membership (E.164-validated, fail-closed). |
| `SignalTransport::send_group` | async fn | Post one body/chunk to a group. Signature: `send_group(&mut self, group_id: &GroupId, body: &str)`. |
| `SignalTransport::create_group` / `quit_group` | async fn | Group lifecycle RPCs. |
| `pending_incoming: VecDeque<Envelope>` | field | **(hardening)** FIFO queue of notifications seen during `request()`; drained by `receive()`. |
| `parse_group_members` | fn | **(hardening)** Parse membership; rejects empty/malformed numbers via `WireError::Membership`. |
| `verify_membership` / `MembershipVerdict` | fn / enum | **(hardening)** Set-equality re-verification kernel: `Verified` when the roster set is unchanged, else `Withhold`. |
| `WireError::Membership` | enum variant | **(hardening)** Fail-closed membership error; message is a fixed reason code and contains no member numbers. (`WireError` also has `Json(String)`.) |
| `config::resolver::validate_e164` | fn | E.164 validator, `fn(&str) -> Result<(), ConfigError>`; **(hardening)** visibility promoted `pub(super)` → `pub(crate)` for transport reuse. |
| `Envelope` | struct | Normalized inbound message (source, group_id, body, is_sync, …). |
| `MAX_FRAME_BYTES` | const | 256 KiB single-frame bound (already present). |

The outbound relay lives in `SignalSession` (`amplihack-signal`), driven by the
CLI `signal` feature:

| Item | Description |
| --- | --- |
| `SignalSession::post_verified` | **(hardening)** Chunked relay that re-verifies membership (`group_members()` + `verify_membership()`) before each `send_group` and withholds fail-closed on mid-body drift. |
| `SignalSession::post` | Single unverified `send_group` mirror post (unchanged). |
| `SignalSession::pump_once` | Inbound step: `receive()` + `Gate::evaluate` (deny-by-default: group scope → non-empty body → echo suppression → allowlist). |
| `gating::Gate` | Deny-by-default inbound evaluation used by `pump_once`. |

---

## Building and testing

Feature-gated build (bridge compiled **in**):

```bash
# transport crate
cargo build -p amplihack-signal --features signal
# CLI driver
cargo build -p amplihack-cli --features amplihack-cli/signal
```

Default build (bridge compiled **out**):

```bash
cargo build            # no `signal` feature → zero bridge code, no extra deps
```

Lints must be clean **both ways**:

```bash
cargo clippy -p amplihack-signal --features signal -- -D warnings
cargo clippy -p amplihack-cli --features amplihack-cli/signal -- -D warnings
cargo clippy -- -D warnings          # feature off
```

### Regression coverage

These tests are **additive** and landed with FIX 1–3 (all under `--features
signal`). They sit alongside the existing transport/channel integration tests
(`transport_frame_bounds_it.rs`, `transport_lossy_decode_it.rs`,
`transport_reconnect_it.rs`, `session_channel_it.rs`, `session_relay_it.rs`).

| Test | Guards |
| --- | --- |
| `crates/amplihack-signal/tests/transport_cancel_safe_it.rs` | A fragmented inbound frame (multiple TCP segments) survives a `receive()` future dropped mid-frame + an intervening `send_group()` request; delivered intact and unduplicated. |
| `crates/amplihack-signal/tests/transport_group_members_it.rs` | Empty and malformed member numbers each reject the whole parse via `WireError::Membership`; error leaks no numbers. |
| `crates/amplihack-signal/tests/transport_membership_verify_it.rs` | `verify_membership` set-equality kernel: reorder/dup `Verified`; removed/altered/injected `Withhold`; parse→verify mid-body pipeline. |
| `crates/amplihack-signal/tests/session_post_verified_it.rs` | A member removed/altered mid-body stops subsequent chunks (`post_verified` withholds) and only pre-drift chunks are sent. |
| `parse_failure_message_does_not_leak_member_numbers` (in `transport_group_members_it.rs`) | Membership error strings never embed member numbers. |

See [Signal Bridge Hardening](signal-bridge-hardening.md) for the threat model
and design rationale, and [Signal Channel](signal-channel.md) for the full
end-to-end channel and onboarding CLI.
