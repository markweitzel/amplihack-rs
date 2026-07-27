# Signal Bridge Hardening

This document specifies the **security hardening** delivered for the Signal
bridge under **Issue #1064** (Signal bridge consolidation). It captures the
threat model, the three fixes, and the fail-closed invariants they establish.

> **Status — implemented.**
> FIX 1–3 are on the branch (feature-gated behind `signal`). Statements about
> "before the fix" describe the prior code; statements about the fix describe the
> shipped behavior. The verification matrix lists the checks that gate the
> change, all currently green.

For the runtime behavior and API surface see [Signal Bridge](SIGNAL_BRIDGE.md);
for the end-to-end channel and onboarding see [Signal Channel](signal-channel.md).

The bridge is feature-gated behind the `signal` Cargo feature (default **OFF**).
All behavior below applies only when built with `--features signal`.

---

## Threat model

The bridge sits on an untrusted boundary: it reads frames from an external
`signal-cli` daemon and relays operator-authored text into a live agent session.
The hardening addresses three concrete risks:

1. **Truncated-frame / lost-message risk.** The inbound reader is intended to
   live in a `tokio::select!` arm (`recv = transport.receive()`). `select!` can
   drop the losing future at any `.await`. The reader as written today clears its
   buffer on entry, so it would *lose* the bytes of a frame that was only
   partially read when the future was dropped — corrupting or silently dropping
   an operator message, or desynchronizing the JSON-RPC stream.

2. **Silent notification loss.** Inbound `receive` notifications can arrive while
   the transport is mid-`request()` (e.g. during a membership check). Discarding
   them silently would drop legitimate operator messages.

3. **Unvalidated / mid-relay membership.** If group membership is trusted
   blindly, a malformed member number could slip through, and — more seriously —
   an attacker added to the group *between* outbound chunks could receive the
   remainder of a relayed message. Leaking member phone numbers into logs is an
   additional PII risk.

**Design principle: fail-closed.** Every ambiguous or adverse condition results
in *withholding* (skip the relay) or *queuing* (never drop), and every withhold
is *surfaced* in logs — never silently swallowed.

---

## FIX 1 — Cancel-safe inbound receive

**Files:** `crates/amplihack-signal/src/transport.rs`

**Problem.** `read_line()` cleared its accumulation buffers (`line_buf`,
`raw_buf`) at the top of every call, so a `receive()` future dropped mid-frame
lost the already-read bytes.

**Fix.**

- **Persist partial-frame state.** `read_line()` no longer clears `raw_buf` /
  `line_buf` on entry. In-progress frame bytes persist across calls, and an
  in-progress oversized-drain persists via a `raw_oversized` flag.
- **Single cancellation point.** The only `.await` is `fill_buf().await`; no
  bytes are consumed at that suspension point. If the future is dropped there,
  nothing is lost.
- **Consume-as-read, reset-after-complete.** Bytes are `consume()`d from the
  socket as they are read (never re-read), but the accumulation buffer is reset
  **only after** a complete frame is returned or an oversized frame is drained.
  A future dropped mid-frame therefore resumes with the accumulated bytes intact.
- **Bounds preserved exactly.** `MAX_FRAME_BYTES` (**256 KiB**, already present)
  still caps a single frame; oversized frames are drained to the next newline and
  surfaced as an empty line for callers to skip (stream resync). The existing
  empty-line resync behavior is unchanged.

**Pending-notification drain.** A new field

```rust
pending_incoming: VecDeque<Envelope>
```

is added to `SignalTransport`. In `request()`, when a frame parses as a
notification (no matching `id`), the parsed `Envelope` is **pushed onto
`pending_incoming`** rather than discarded — fail-closed against silent loss.
`receive()` **drains `pending_incoming` first** (FIFO) before reading a new
frame. Notifications interleaved with a `request()` are thus queued and later
delivered in order, exactly once.

**Bridge impact: none/minimal.** `receive()` remains the intended `select!` arm
(`recv = transport.receive()`). There is **no** mpsc channel and **no** spawned
reader task. All gating and membership semantics are preserved.

**Invariant.** A fragmented inbound frame delivered across multiple TCP segments
is delivered **intact and unduplicated**, even if the `receive()` future is
dropped mid-frame and an intervening `request()` / `group_members()` call runs.

**Regression test.**
`crates/amplihack-signal/tests/transport_cancel_safe_it.rs` uses the same real
`TcpListener` chunked-write seam as the existing `transport_frame_bounds_it.rs`
/ `transport_reconnect_it.rs` tests: it delivers one inbound frame in multiple
TCP segments while a competing event drops the `receive()` future mid-frame (with
an intervening `send_group()` request that observes and queues the completed
notification), and asserts the fragmented `Envelope` is ultimately delivered
intact and unduplicated.

---

## FIX 2 — E.164 fail-closed group membership

**Files:** `crates/amplihack-signal/src/transport.rs` (new `group_members` /
`parse_group_members`, new `WireError::Membership` variant),
`crates/amplihack-signal/src/config/resolver.rs` (`validate_e164` visibility).

**Problem.** There was no `group_members` / `parse_group_members` on the
transport, and `WireError` had a single variant (`Json(String)`). Membership
numbers were not validated, and there was no fail-closed parse to reject
malformed/empty entries.

**Fix.**

- Add `SignalTransport::group_members()` and a `parse_group_members` helper that
  validates every member number with the **in-crate** validator
  `amplihack_signal::config::resolver::validate_e164` — `+` followed by
  **1..=15 ASCII digits**.
- **Reuse the existing validator; do not fork it.** `validate_e164` had signature
  `fn validate_e164(s: &str) -> Result<(), ConfigError>` and visibility
  `pub(super)`. It was promoted to `pub(crate)` so both the resolver and the
  transport call the *same* validator. `parse_group_members` discards the
  validator's `ConfigError` (which embeds the number) and surfaces the new
  `WireError::Membership` variant instead.
- **Add `WireError::Membership`.** `WireError` gained a second, fail-closed
  variant alongside `Json(String)`. It carries a fixed `&'static str` *reason
  code* (e.g. `"empty member set"`, `"non-conforming member number"`) and never a
  member number, so both its `Display` and `Debug` output are PII-free.
- **No cross-crate import.** The bridge does **not** import validation from
  `amplihack-cli`; that would create a circular dependency (the only allowed
  direction is `cli → signal`).
- **Reject on first offender.** In the member loop, the **first** empty or
  non-conforming number rejects the whole parse, returning `WireError::Membership`.
- **No PII leakage.** The error message never embeds member numbers; a new
  `parse_failure_message_does_not_leak_member_numbers` unit test enforces this.

**Regression test.**
`crates/amplihack-signal/tests/transport_group_members_it.rs` covers (a) an
**empty** member set, (b) an **empty** number, and (c) a **malformed** number,
each of which rejects the whole parse via `WireError::Membership`, plus
`parse_failure_message_does_not_leak_member_numbers`, which re-asserts no number
appears in the error's `Display` or `Debug`.

---

## FIX 3 — Per-post membership re-verification

**Files:** `crates/amplihack-signal/src/session_channel.rs`
(`SignalSession::post_verified`), `crates/amplihack-signal/src/transport.rs`
(`verify_membership` / `MembershipVerdict`).

**Problem.** `SignalSession::post` performs a single `send_group` with no
chunking and no membership re-check, so a large body verified once up-front could
have later chunks posted to a group whose membership changed mid-relay.

**Fix.** A new `SignalSession::post_verified(&[&str])` takes the caller-split
chunks, snapshots the roster once via `transport.group_members()` (FIX 2), and
**immediately before EACH chunk** re-reads `group_members()` and re-checks it
against the snapshot with `transport::verify_membership`. `verify_membership`
compares the two rosters as **set equality** (order- and duplicate-insensitive),
returning `MembershipVerdict::Verified` when the set is unchanged and
`MembershipVerdict::Withhold` on any drift — a member removed, altered, or
injected. `send_group` takes `&GroupId` (not `&str`):
`send_group(&mut self, group_id: &GroupId, body: &str)`. On a `Withhold`:

- Remaining chunks are **not** sent (fail-closed) and the call returns an error.
- The withheld relay is logged via a `WITHHOLDING` line (`tracing::warn!` plus an
  `eprintln!`) — surfaced, never silently dropped, and carrying no member numbers
  or body text.

The existing `post()` (single unverified `send_group`) is left unchanged;
`post_verified()` is the additive fail-closed relay path. No caps or rate limits
are added; the change is purely verify-then-send, chunk by chunk.

**Regression test.**
`crates/amplihack-signal/tests/session_post_verified_it.rs` (alongside the
existing `session_channel_it.rs` / `session_relay_it.rs`) drives
`post_verified` over the offline `FakeSignalEndpoint`: a stable roster relays
every chunk in order, while a member dropped mid-body (via the fake's
`drop_member_after_send` seam) stops subsequent chunks — only the pre-drift chunk
is sent. The pure `verify_membership` kernel is locked separately by
`transport_membership_verify_it.rs`.

---

## Fail-closed invariants (summary)

| Condition | Outcome | Surfaced? |
| --- | --- | --- |
| `receive()` future dropped mid-frame | Bytes persist; frame resumes intact next call | n/a (no loss) |
| Notification during `request()` | Queued in `pending_incoming` (FIFO), delivered later | n/a (no loss) |
| Frame exceeds 256 KiB | Drained to next newline, skipped (stream resync) | n/a (fail-safe) |
| Empty member number | Whole parse rejected (`WireError::Membership`) | error (no PII) |
| Malformed member number | Whole parse rejected (`WireError::Membership`) | error (no PII) |
| Membership changes mid-body | Remaining chunks withheld | `WITHHOLDING` log |

No condition results in a silent drop or a silently truncated relay.

---

## Verification matrix

These are the checks that gate FIX 1–3; all are green on the branch.

| Check | Command | Result |
| --- | --- | --- |
| Build (signal on) | `cargo build -p amplihack-signal --features signal` | pass |
| Build (CLI, signal on) | `cargo build -p amplihack-cli --features amplihack-cli/signal` | pass |
| Build (default off) | `cargo build` | pass (bridge compiled out) |
| Clippy (signal on) | `cargo clippy -p amplihack-signal --features signal --all-targets -- -D warnings` | clean |
| Clippy (off) | `cargo clippy -p amplihack-signal --all-targets -- -D warnings` | clean |
| Clippy (CLI, signal on) | `cargo clippy -p amplihack-cli --features amplihack-cli/signal --all-targets -- -D warnings` | clean |
| Signal tests | `cargo test -p amplihack-signal --features signal` | existing pass (unweakened) + new regressions |

---

## Constraints and scope

The hardening is deliberately narrow:

- Tracked under **Issue #1064** on the Signal bridge consolidation branch.
- **No** version bump, **no** caps or rate limits, **no** `--no-verify`, **no**
  auto/admin merge.
- Existing signal tests remain **unweakened**; new tests are additive.
- Dependency direction stays `cli → signal` (validation kept in-crate; the
  `validate_e164` reuse is a same-crate visibility promotion, not a cross-crate
  import).

For runtime usage and the full API surface, see
[Signal Bridge](SIGNAL_BRIDGE.md).
