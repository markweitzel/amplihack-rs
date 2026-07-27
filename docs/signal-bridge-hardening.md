# Signal Bridge Hardening

**Issue #1064 — harden the Signal bridge.** This document is the design and
verification specification for three defensive fixes to the
[`amplihack-signal`](../crates/amplihack-signal) transport and relay. It
describes the behavior: the chosen semantics, the API surface, the
security rationale, and the regression tests that lock each property in
place.

> **Status: implemented.** All three fixes are implemented on the
> `amplihack-signal` crate and covered by passing regression tests
> (`group_membership_it`, `session_membership_reverify_it`,
> `transport_cancel_safe_it`, plus the existing transport/relay suites).
> Verified against the tree: `validate_e164` is `pub(crate)` and reused by the
> transport; `WireError` has a number-free `Membership` variant;
> `read_line()` persists `raw_buf`/`frame_oversized` across calls and resets
> only at a frame boundary; `SignalTransport` has a `pending_incoming` queue,
> `parse_group_members`, and a `group_members()` method;
> `SignalSession::post` re-verifies membership before sending; and `Gate`
> exposes the `outbound_members_authorized` predicate.

For the operator-facing summary, see [Signal Bridge](SIGNAL_BRIDGE.md). For the
whole channel, see [Signal Channel](signal-channel.md).

- **Crate under change:** `amplihack-signal`
- **Cargo feature:** `signal` (default **OFF**)
- **Files:** `src/transport.rs`, `src/config/resolver.rs`, `src/config.rs`,
  `src/session_channel.rs`, `src/gating.rs`
- **Consumer:** `amplihack-cli` (feature `amplihack-cli/signal`), which drives
  `SignalTransport::receive()` as a `tokio::select!` arm

---

## Contents

- [Threat model](#threat-model)
- [FIX 1 — cancel-safe inbound receive](#fix-1--cancel-safe-inbound-receive)
- [FIX 2 — E.164-validated group membership](#fix-2--e164-validated-group-membership)
- [FIX 3 — per-post membership re-verification](#fix-3--per-post-membership-re-verification)
- [API surface changes](#api-surface-changes)
- [Test matrix](#test-matrix)
- [Build & verification](#build--verification)
- [Non-goals](#non-goals)

---

## Threat model

The bridge straddles a trust boundary: untrusted Signal traffic on one side, a
running agent session on the other. Three concrete failure modes are closed:

| # | Failure mode | Consequence if unhardened |
| - | ------------ | ------------------------- |
| 1 | A `receive()` future is dropped between TCP segments of one frame | Lost or duplicated operator messages across a trust boundary |
| 2 | Group membership contains an empty/malformed number | Relay operates on an unvalidated member set (spoof / injection surface) |
| 3 | Membership changes after the session was authorized (new member added) | A newly-added, non-allowlisted member receives every later session update they were never authorized to see (TOCTOU) |

Guiding principles: **fail closed** on every unknown path, **never leak PII** in
logs or errors, **never cache authorization**, and keep the whole subsystem
**compiled out** unless `--features signal` is set.

---

## FIX 1 — cancel-safe inbound receive

**File:** `crates/amplihack-signal/src/transport.rs` (only).

### Problem

`SignalTransport::receive()` is polled as one arm of the session's
`tokio::select!` loop. When a competing arm wins, the runtime **drops the
`receive()` future** wherever it is currently suspended — which may be partway
through reading a multi-segment frame. The pre-hardening `read_line()` cleared
its accumulation buffers (`raw_buf`, `line_buf`) at the **top** of every call,
so a dropped-then-restarted read discarded any bytes already consumed from the
socket: silent message loss. Separately, `request()` **discarded** any inbound
notification that arrived while it awaited its matching `id` response: silent
message drop.

### Behavior (finished state)

**1. Persist the in-progress frame across calls.**
`read_line()` does **not** clear `raw_buf`/`line_buf` on entry. It consumes
bytes from the `BufReader` as it reads them (so the OS/socket buffer is
advanced), accumulating them into `raw_buf`. The accumulation buffer is reset
**only after** a terminal outcome is produced:

- a complete newline-terminated frame is decoded and returned, **or**
- an oversized frame is drained to the next newline and the empty-line sentinel
  is returned.

If the future is dropped before either terminal outcome, the already-consumed
bytes remain in `raw_buf`. The next `read_line()` call resumes accumulation on
top of them and completes the same frame — delivered **intact** and **exactly
once**.

**2. Queue interleaved notifications instead of discarding them.**
`SignalTransport` gains a field:

```rust
pending_incoming: VecDeque<Envelope>,
```

In `request()`, when a fully-read frame parses successfully but is a
*notification* (its `id` does not match the in-flight request `id`), the parsed
`Envelope` is **pushed onto `pending_incoming`** rather than dropped. `request()`
continues waiting for its own `id`.

**3. Drain the queue first in `receive()`.**
`receive()` returns any queued `Envelope` from the front of `pending_incoming`
**before** reading a new frame from the socket. Ordering is FIFO, so a
notification that arrived during a request is delivered on the next `receive()`
in arrival order.

### Preserved invariants

- `MAX_FRAME_BYTES = 256 * 1024` (256 KiB) frame bound is unchanged.
- Oversized frames are still drained to the next newline and reported as an
  empty line (`Ok(Some(""))`) that callers skip — the resync behavior is
  byte-for-byte identical.
- Empty-line / non-JSON lines are still skipped fail-safe.
- `receive()` remains a plain `select!` arm: **no** mpsc channel, **no** spawned
  reader task. All gating and membership semantics downstream are untouched.

### Cancel-safety as a security property

Cancellation-loss here is not a mere robustness nit: a dropped `receive()`
across the Signal↔agent trust boundary would either **lose** an operator
instruction or, under a naive re-read, **duplicate** one. Persisting partial
frames and queueing notifications makes delivery lossless and non-duplicating,
which is required for the channel's at-most-once advisory semantics.

---

## FIX 2 — E.164-validated group membership

**Files:** `crates/amplihack-signal/src/transport.rs`,
`crates/amplihack-signal/src/config/resolver.rs`,
`crates/amplihack-signal/src/config.rs`.

### Single source of truth

The E.164 rule already lives in the config resolver:

```rust
// crates/amplihack-signal/src/config/resolver.rs
pub(crate) fn validate_e164(s: &str) -> Result<(), ConfigError> {
    let ok = s.starts_with('+') && {
        let digits = &s[1..];
        !digits.is_empty() && digits.len() <= 15 && digits.bytes().all(|b| b.is_ascii_digit())
    };
    if ok { Ok(()) } else { Err(ConfigError::InvalidE164(s.to_string())) }
}
```

Its visibility is promoted from `pub(super)` to **`pub(crate)`**, and the
resolver module is exposed to the crate via `pub(crate) mod resolver;` in
`config.rs`. `transport.rs` reuses **this** predicate. It does **not** define a
second `is_e164`, and it does **not** import any validator from `amplihack-cli`
(no upward dependency). The rule is exactly `+` then **1..=15 ASCII digits** —
ASCII-only, so Unicode/homoglyph digits are rejected.

> Because `validate_e164` returns `Result<(), ConfigError>` (not `bool`), the
> membership check treats `validate_e164(n).is_ok()` as the acceptance
> predicate.

### Fail-closed parse

`transport::parse_group_members` iterates the members reported by the daemon and
rejects on the **first** offending entry:

- an **empty** number, or
- a number that fails `validate_e164`,

returning the new fail-closed `WireError::Membership`. One bad member fails
the **entire** parse — the bridge never proceeds on a partially-validated set.

### No PII in errors

`WireError::Membership` is a **field-less** variant that renders a fixed message
with no phone number, no count, and no other member data:

```rust
#[derive(Debug, thiserror::Error)]
pub enum WireError {
    #[error("invalid JSON frame: {0}")]
    Json(String),
    /// Group membership failed E.164 validation. Field-less by design so no
    /// member number can leak through the error path.
    #[error("group membership rejected")]
    Membership,
}
```

A new regression, `parse_failure_message_does_not_leak_member_numbers`, will
assert the rendered string contains no member number (it is not present in the
tree today).

---

## FIX 3 — per-post membership re-verification

**Files:** `crates/amplihack-signal/src/transport.rs` (new `group_members()` +
`parse_group_members`), `crates/amplihack-signal/src/session_channel.rs` (relay),
`crates/amplihack-signal/src/gating.rs` (new outbound predicate).

### Problem

Group membership is mutable: an operator can add a participant to the Signal
group at any point, including after the session started and was authorized.
`SignalSession::post` currently calls `send_group()` (a single
`request("send", …)`) with **no** membership check at all. If a non-allowlisted
member is added mid-session, every subsequent post is delivered to them — a
time-of-check/time-of-use (TOCTOU) leak across the trust boundary.

There is no chunking involved and none is introduced: each outbound update is
one `send_group()` call, so the correct granularity for closing the window is
**per post**.

### Behavior (finished state)

**1. New live-membership fetch.** `SignalTransport` gains

```rust
pub async fn group_members(&mut self, group_id: &GroupId)
    -> Result<Vec<String>, WireError>;
```

which issues a signal-cli `listGroups` request for the session group and parses
the returned member roster through `parse_group_members` — the FIX 2 fail-closed
E.164 validator. Any empty/malformed member fails the whole call with
`WireError::Membership`.

**2. New outbound authorization predicate.** `Gate` gains a member-set check
that reuses the **same** allowlist `HashSet` it already holds:

```rust
/// True iff every live group member is on the allowlist.
/// An empty allowlist denies (consistent with inbound gating).
#[must_use]
pub fn outbound_members_authorized(&self, members: &[String]) -> bool;
```

This is a **new** predicate, deliberately separate from `Gate::evaluate` (which
authorizes a single *inbound* `Envelope` by sender + device + echo window). The
two directions share only the allowlist data, not the decision logic.

**3. Re-verify before every post.** `SignalSession::post` (and `announce`, which
delegates to it) becomes:

1. call `self.transport.group_members(&self.group_id)` (live fetch, FIX 2 parse);
2. call `self.gate.outbound_members_authorized(&members)`;
3. **only if authorized**, call `send_group()` and `record_outbound()`;
4. on an unauthorized member set **or** a `group_members()` error:
   - **withhold** — do not send,
   - **log** via the existing `WITHHOLDING` pattern (`eprintln!` / `tracing`),
     recording the *decision* only (no numbers), and
   - **fail closed** — return an error instead of delivering.

Membership is re-fetched live on every post; it is **never cached or persisted**
between posts (caching would re-open the window). **No caps** are introduced.

---

## API surface changes

| Item | Before | After |
| ---- | ------ | ----- |
| `resolver::validate_e164` | `pub(super) fn … -> Result<(), ConfigError>` | `pub(crate) fn …` (single shared predicate) |
| `config::resolver` module | `mod resolver;` | `pub(crate) mod resolver;` |
| `SignalTransport` field | — | `pending_incoming: VecDeque<Envelope>` |
| `SignalTransport::read_line` | clears buffers on entry | persists partial frame; resets only after a complete frame / oversized-drain |
| `SignalTransport::request` | drops interleaved notifications | pushes non-matching-`id` notifications into `pending_incoming` |
| `SignalTransport::receive` | reads a fresh frame each call | drains `pending_incoming` first, then reads |
| `transport::parse_group_members` | — | fail-closed E.164 parse ⇒ `WireError::Membership` |
| `SignalTransport::group_members` | — | new `listGroups` fetch → `parse_group_members` (live roster) |
| `WireError` | `Json(String)` | adds field-less `Membership` variant (fixed, PII-free message) |
| `Gate::outbound_members_authorized` | — | new predicate: every live member on the shared allowlist (empty ⇒ deny) |
| `SignalSession::post` / `announce` | sends with no membership check | re-fetch `group_members()` + `outbound_members_authorized` recheck before send; `WITHHOLDING` fail-closed |

All additions are gated behind `--features signal`; the default (feature-off)
build is unchanged and pulls in no new dependencies.

---

## Test matrix

| Test | File | Asserts |
| ---- | ---- | ------- |
| Fragmented delivery survives mid-frame cancel | `crates/amplihack-signal/tests/transport_cancel_safe_it.rs` | One inbound frame split across multiple TCP segments (real `TcpListener` chunked-write seam) is delivered **intact and unduplicated** even when a competing event drops `receive()` mid-frame and an intervening `request()`/`group_members()` call runs |
| Notification queued during request | `crates/amplihack-signal/tests/transport_cancel_safe_it.rs` | A notification arriving while `request()` awaits its `id` is queued and later drained by `receive()`, never dropped |
| Frame bound preserved | `crates/amplihack-signal/tests/transport_frame_bounds_it.rs` | Oversized frame is drained + resynced; `MAX_FRAME_BYTES` still enforced |
| Empty member rejected | `crates/amplihack-signal/src/config/tests.rs` | An empty member number rejects the whole `parse_group_members` |
| Malformed member rejected | `crates/amplihack-signal/src/config/tests.rs` | A non-E.164 member number rejects the whole parse |
| Error leaks no numbers | new `parse_failure_message_does_not_leak_member_numbers` | `WireError::Membership` message contains no member numbers (test to be added) |
| Unauthorized member withholds post | `crates/amplihack-signal/tests/session_relay_it.rs` | A non-allowlisted member added to the live roster makes `post()` **withhold** (no send), log the `WITHHOLDING` decision, and return an error |
| Authorized roster still posts | `crates/amplihack-signal/tests/session_relay_it.rs` | With every live member allowlisted, `post()` sends normally and records the outbound echo |

New regression tests are **added**; no existing signal test is weakened or
removed.

---

## Build & verification

The change set is verified in **both** feature modes.

```bash
# Feature ON
cargo build  -p amplihack-signal --features signal
cargo build  -p amplihack-cli    --features amplihack-cli/signal
cargo clippy -p amplihack-signal --features signal -- -D warnings
cargo clippy -p amplihack-cli    --features amplihack-cli/signal -- -D warnings
cargo test   -p amplihack-signal --features signal

# Feature OFF (default)
cargo build
cargo clippy --all-targets -- -D warnings
```

Requirements:

- `clippy -D warnings` is clean **both** ways (removing the duplicated local
  predicate must not leave dead code).
- All existing signal tests pass **unweakened**; new regression tests pass.
- Delivered via: commit → pre-commit (`fmt` + `clippy`) green → push.

---

## Non-goals

Explicitly out of scope for this hardening:

- **No version bump.**
- **No outbound chunking / no send-path rearchitecture** — `send_group()` stays
  a single `request("send", …)`; re-verification is strictly per-post, not
  per-chunk.
- **No new caps** (no per-body chunk cap, no membership-size cap beyond the
  existing frame bound).
- **No `--no-verify`, no auto/admin merge.**
- **No new persistence** — all transport/relay state stays in volatile buffers
  (`raw_buf`, `line_buf`, `pending_incoming`); adding storage would require a
  fresh data-at-rest threat model.
- **No mpsc/spawned-task rearchitecture** — `receive()` stays a `select!` arm.

---

## Related documentation

- [Signal Bridge](SIGNAL_BRIDGE.md) — operator-facing behavioral summary.
- [Signal Channel](signal-channel.md) — end-to-end channel, gating, security.
- [Signal external integration](signal-external-integration.md) — embedding.
