# Signal Bridge

The **Signal bridge** is the hardened inbound/outbound relay at the heart of the
[Signal channel](signal-channel.md). It carries operator messages from a private
Signal group into a running amplihack session and mirrors the session's outbound
updates back to the group, over the signal-cli JSON-RPC 2.0 transport
(`amplihack-signal::transport::SignalTransport`).

> **Status: implemented (Issue #1064).** This page describes the hardened
> behavior, which is now in place: `read_line()` persists its partial-frame
> buffer across calls, `SignalTransport` has a `pending_incoming` queue,
> `validate_e164` is `pub(crate)` and reused by the transport, `WireError` has a
> number-free `Membership` variant, `group_members()` / `parse_group_members`
> exist, and `SignalSession::post` re-verifies membership before every send.
> Each property is locked by a passing regression test.

This page documents the **behavioral contract** of the bridge for the
Issue #1064 hardening work. Three properties are guaranteed:

1. **Cancel-safe inbound receive** — a `receive()` future that is dropped
   mid-frame (e.g. by a losing `tokio::select!` arm) never loses or duplicates
   bytes, and notifications that arrive while a request is in flight are queued,
   never dropped.
2. **E.164-validated group membership** — group membership is parsed through a
   single fail-closed E.164 predicate; a malformed or empty member number
   rejects the whole parse, and no member number is ever leaked in an error.
3. **Per-post membership re-verification** — before **every** outbound post the
   relay re-fetches live group membership and re-checks it against the
   allowlist, and withholds the post fail-closed the instant an unauthorized
   member is present.

- **Crate:** `amplihack-signal`
- **Cargo feature:** `signal` (default **OFF**; zero cost when off)
- **Primary modules:** `transport.rs`, `gating.rs`, `session_channel.rs`,
  `config/resolver.rs`

> For the detailed design, API surface, and test matrix behind these
> guarantees, see [Signal bridge hardening](signal-bridge-hardening.md).

---

## Contents

- [Where the bridge sits](#where-the-bridge-sits)
- [Cancel-safe inbound receive](#cancel-safe-inbound-receive)
- [E.164-validated group membership](#e164-validated-group-membership)
- [Per-post membership re-verification](#per-post-membership-re-verification)
- [Guarantees at a glance](#guarantees-at-a-glance)
- [Related documentation](#related-documentation)

---

## Where the bridge sits

The bridge is not a separate process or task. `SignalTransport::receive()` is
driven directly as one arm of the session's `tokio::select!` loop, alongside the
competing timers and outbound-post arms. There is **no** mpsc channel and **no**
spawned reader task between the socket and the select loop — the transport is
polled cooperatively and must therefore be *cancel-safe*.

```text
Signal group  ─┐                         ┌─►  file inbox  ─►  agent (advisory)
               │  signal-cli JSON-RPC     │
   operator ──►│◄──── TCP (NDJSON) ──────►│  SignalTransport
               │                          │   ├─ receive()       (inbound select arm)
   agent   ───►│                          └─►  ├─ send_group()    (outbound, single send)
 (session) ────┘                              └─ group_members()  (membership, new)
```

Every property below is a property of `SignalTransport` and the
`SignalSession` relay that wraps it.

---

## Cancel-safe inbound receive

signal-cli delivers a single logical frame as one or more TCP segments. Because
`receive()` is a select arm, the runtime may **drop the future between
segments** whenever a competing arm becomes ready. The bridge guarantees that
this cancellation is lossless.

**Persisted partial-frame state.** `read_line()` no longer clears its
accumulation buffer on entry. Bytes are consumed from the socket as they are
read, but the in-progress frame is **persisted across calls**; the buffer is
reset only *after* a complete newline-terminated frame (or an oversized-drain)
has been returned. A future dropped mid-frame therefore resumes on the next poll
with the already-received bytes intact — no truncation, no re-read, no
duplication.

**Pending-notification drain.** While `request()` is awaiting the JSON-RPC
response that matches its `id`, inbound frames that parse as *notifications*
(any frame with no matching request `id`) are no longer discarded. They are
parsed into `Envelope`s and pushed onto an internal
`pending_incoming: VecDeque<Envelope>` queue. `receive()` **drains
`pending_incoming` first**, before reading any new frame from the socket. The
net effect: a notification that arrives interleaved with an in-flight request
(for example, an operator message that lands during a `group_members()` call) is
queued and delivered in order on the next `receive()`, never dropped.

**Preserved bounds and resync.** The 256 KiB `MAX_FRAME_BYTES` cap, the
oversized-drain-to-next-newline behavior, and the empty-line resync semantics
are unchanged. An oversized or never-terminated frame is still drained and
reported as an empty (skipped) line, so a hostile peer cannot drive unbounded
memory growth.

See [hardening § FIX 1](signal-bridge-hardening.md#fix-1--cancel-safe-inbound-receive)
for the buffer state machine and the `transport_cancel_safe_it.rs` regression
test.

---

## E.164-validated group membership

When the bridge fetches the live membership of a group, every member number is
validated through a **single source-of-truth predicate**:
`amplihack_signal::config::resolver::validate_e164` (promoted to `pub(crate)`
for in-crate reuse). A number is valid iff it is `+` followed by **1..=15 ASCII
digits** — exactly the same rule already enforced on the configured allowlist.
The bridge does **not** import any validator from `amplihack-cli`, and it does
**not** define a second copy of the rule.

**Fail-closed parsing.** `transport::parse_group_members` rejects on the **first**
empty or non-conforming member number and returns the fail-closed
`WireError::Membership`. A single bad member fails the **whole** parse — the
bridge never operates on a partially-validated member set.

**No number leakage.** The `WireError::Membership` message is a fixed string; it
carries **no** phone numbers, member counts, or other PII. (A new regression,
`parse_failure_message_does_not_leak_member_numbers`, will lock this in — it does
not yet exist in the tree.)

See [hardening § FIX 2](signal-bridge-hardening.md#fix-2--e164-validated-group-membership).

---

## Per-post membership re-verification

Group membership is not static: an operator can add a participant to the Signal
group at any time, including *after* the session was authorized. Trusting the
membership captured at session start would leave a time-of-check/time-of-use
window — a newly-added, non-allowlisted member would receive every subsequent
session update.

The bridge closes that window by re-verifying membership on the outbound path.
`SignalSession::post` (and `announce`) re-runs a new
`transport.group_members()` call — which parses the live roster through the
FIX 2 fail-closed validator — and re-checks the fresh member set against the
configured allowlist **immediately before each send**, never once per session.
The check is a **new** outbound predicate that shares the same allowlist
`HashSet` as the inbound `Gate`; it is distinct from `Gate::evaluate`, which
authorizes *inbound* senders per `Envelope` and is not reused here. A group is
authorized to post to iff **every** live member is on the allowlist (an empty
allowlist denies, consistent with inbound gating).

On **any** verification failure it:

1. **withholds** — the update is **not** sent;
2. **logs the withheld post** using the existing `WITHHOLDING` log pattern
   (`eprintln!` / `tracing`), recording the decision only (no numbers); and
3. **fails closed** — returns an error rather than delivering to a group with an
   unauthorized member.

Membership is re-fetched live for every post; it is never cached or persisted
between posts, and **no caps** are introduced. This fix deliberately does **not**
chunk outbound bodies — `send_group()` remains a single send, so re-verification
is strictly per-post.

See [hardening § FIX 3](signal-bridge-hardening.md#fix-3--per-post-membership-re-verification).

---

## Guarantees at a glance

| Property | Guarantee | Mechanism |
| --- | --- | --- |
| Inbound loss | A `receive()` dropped mid-frame loses **no** bytes | Persisted partial-frame buffer in `read_line()` |
| Inbound drop | Notifications during a request are **never** dropped | `pending_incoming: VecDeque<Envelope>`, drained first |
| Inbound duplication | A resumed frame is delivered **exactly once** | Buffer reset only after a complete frame / drain |
| Memory DoS | Oversized frames cannot exhaust memory | `MAX_FRAME_BYTES` (256 KiB) + oversized-drain resync |
| Membership | Malformed/empty member ⇒ whole parse **rejected** | `validate_e164` (fail-closed `WireError::Membership`) |
| PII | Membership errors leak **no** numbers | Fixed error string, no member data |
| TOCTOU | A member added mid-session is caught before the next post | Per-post `group_members()` re-verify + `WITHHOLDING` fail-closed |

All guarantees hold only with `--features signal`; with the feature off the
bridge is compiled out entirely.

---

## Related documentation

- [Signal bridge hardening](signal-bridge-hardening.md) — design, API surface,
  and regression-test matrix for the three fixes above.
- [Signal Channel](signal-channel.md) — the end-to-end channel, config schema,
  gating rules, and security model.
- [Signal external integration](signal-external-integration.md) — embedding the
  channel in another host.
- [Signal Onboarding](SIGNAL_ONBOARDING.md) — `amplihack signal setup`.
