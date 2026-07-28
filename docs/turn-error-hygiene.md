# Turn-Failure Error Hygiene

When a Copilot turn's child process exits non-zero, the driver in
[`crates/amplihack-turn`](../crates/amplihack-turn) returns an error that
describes the failure **without** dumping the child's full stdout/stderr into
the surfaced message. By default the error carries only the exit status and a
short, size-bounded tail of the combined output. The full output is still
available to operators who opt into debug-level logging.

This closes the information-disclosure and log-hygiene concern in issue #1092:
raw child output can contain secrets, tokens echoed by tools, absolute paths,
or many megabytes of log text, and this error string is both written to logs
and relayed onward through the Signal chat layer.

As a defense-in-depth measure (issue #1108), the failure path also routes the
combined output through an **optional, injected redactor** before it is used for
either the debug log or the bounded tail. When a redactor is configured, secrets
matching its patterns are masked in **both** sinks — the `tracing::debug!`
`output` field and the bounded error tail — with a single redaction pass. When
no redactor is configured, the failure path is an exact no-op (identity), so the
always-compiled `amplihack-turn` crate stays free of regex and redaction
dependencies. See [The injected redactor](#the-injected-redactor).

Read this document when you need to:

- understand what a failed turn's error message contains and why,
- configure how much trailing output that message includes,
- inject a redactor so secrets are masked in the error tail and debug output,
- retrieve the full child output while diagnosing a failure,
- write or read tests that assert the failure-error contract.

---

## Contents

- [Overview](#overview)
- [What the error contains](#what-the-error-contains)
- [The injected redactor](#the-injected-redactor)
- [Configuration: `AMPLIHACK_TURN_ERROR_TAIL_BYTES`](#configuration-amplihack_turn_error_tail_bytes)
- [Retrieving the full output at debug level](#retrieving-the-full-output-at-debug-level)
- [API reference](#api-reference)
- [Examples](#examples)
- [Design notes](#design-notes)
- [Security invariants](#security-invariants)
- [Testing](#testing)
- [See also](#see-also)

---

## Overview

The production turn runner, `CopilotTurnRunner`, spawns each `copilot` turn as a
child process and captures its stdout and stderr. There are two outcomes:

- **Success (zero exit).** The runner returns the child's captured stdout
  verbatim as the turn output. This path is unchanged — full stdout is the
  turn result, exactly as before.
- **Failure (non-zero exit).** The runner returns an `io::Error` whose message
  is a **summary**: the exact prefix `copilot turn failed ({status})` followed
  by only a bounded tail of the combined stdout+stderr. The complete combined
  output is emitted separately at `tracing::debug!`.

Only the non-zero-exit path changed. The failure error still begins with the
stable prefix `copilot turn failed`, so existing log parsing and the chat
layer's `turn failed: {e}` relay message keep working.

---

## What the error contains

On a non-zero exit the returned error string has the form:

```text
copilot turn failed ({status}); last {n} bytes of output: {tail}
```

- `{status}` — the child's exit status, e.g. `exit status: 3`. The
  `copilot turn failed ({status})` prefix is preserved exactly and is stable
  for downstream parsing.
- `{n}` — the **actual** number of bytes in `{tail}` after the char-boundary
  snap (see below). This is the truthful length of what follows, not the
  configured budget.
- `{tail}` — the last `n` bytes of the combined output, built as the child's
  stdout followed by its stderr (preserving the historical ordering), decoded
  losslessly (`from_utf8_lossy`).

Behavioral details:

- **Bounded.** `{tail}` is at most the configured budget in bytes (see
  [configuration](#configuration-amplihack_turn_error_tail_bytes)). The error
  message length no longer scales with child output size, so a chatty or
  runaway child cannot flood logs or relayed messages.
- **Short output.** If the combined output is already within the budget, the
  whole output is included and `{n}` equals its full length.
- **Multibyte-safe.** The tail start index is snapped **forward** to the
  nearest UTF-8 character boundary, so the tail never splits a multibyte
  character and never panics. Because the snap moves forward, the tail is
  always `<= budget` bytes.
- **Redacted before bounding.** When a redactor is injected (see
  [The injected redactor](#the-injected-redactor)), the combined output is
  scrubbed **once, before** the tail is cut, so a secret straddling the tail
  boundary cannot leak its trailing half. With no redactor injected the tail is
  bounded but not scrubbed, exactly as before — the default is a true no-op.

---

## The injected redactor

The failure path can route the combined child output through a caller-supplied
redactor before that output reaches **either** log sink. This is a
defense-in-depth seam (issue #1108): even if a tool echoes a secret into the
last few kilobytes of a failing turn, a configured redactor masks it in both the
surfaced error tail and the debug `output` field.

### Design: an injected closure, default no-op

`amplihack-turn` is compiled in **every** build and is intentionally lean, so it
must not depend on `amplihack-signal` or pull in a regex engine. Instead of a
hard dependency, the runner exposes an **injection seam**: an optional closure.

```rust
redactor: Option<std::sync::Arc<dyn Fn(&str) -> String + Send + Sync>>
```

- The closure type is `Arc<dyn Fn(&str) -> String + Send + Sync>`. `Arc` (not
  `Box`) and the `Send + Sync` bounds are required because the runner's
  `run_argv` future is `Send` and the closure is cloned into that future, so it
  must cross the `await` boundary.
- **Default is `None`.** `CopilotTurnRunner::new(program, preempt)` leaves the
  redactor unset. With `None`, the failure path performs **no** allocation and
  **no** call — it is a genuine identity no-op. The success (zero-exit) hot path
  never touches the redactor at all.
- The redactor is expected to be **total and infallible** (it returns a
  `String`, never panics). The production redactor is a set of regex replaces,
  which is panic-free.

### Injecting a redactor

Use the builder method `with_redactor`:

```rust
use std::sync::Arc;
use amplihack_turn::turn::CopilotTurnRunner;

let runner = CopilotTurnRunner::new("copilot", preempt.clone())
    .with_redactor(Arc::new(amplihack_signal::chat::outbound::redact_for_relay));
```

`with_redactor` is `#[must_use]` and returns the runner by value (builder
style). Calling it more than once keeps the last redactor supplied.

### Redact-once, before bounding

On a non-zero exit the runner:

1. Builds `combined` = child stdout followed by child stderr (lossy UTF-8).
2. If a redactor is set, replaces `combined` with `redactor(&combined)` **once**.
3. Emits the `tracing::debug!` `output` field from that (possibly redacted)
   `combined`.
4. Computes the bounded tail (`char_boundary_tail`) from the **same** redacted
   `combined`, and reports `n = tail.len()` on the final tail.

Because a single redaction pass feeds both sinks and runs **before** the tail is
cut, there is exactly one place a secret could leak — and both are covered. The
error prefix `copilot turn failed ({status})` and the
`; last {n} bytes of output: {tail}` shape are unchanged; only the tail
**content** is scrubbed.

### Production wiring

The Signal chat relay path injects the real relay redactor,
`amplihack_signal::chat::outbound::redact_for_relay`, at the
`CopilotTurnRunner` construction site
(`crates/amplihack-cli/src/commands/signal/chat.rs`):

```rust
let driver = SerialTurnDriver::new(
    CopilotTurnRunner::new(COPILOT_BIN, preempt.clone())
        .with_redactor(std::sync::Arc::new(
            amplihack_signal::chat::outbound::redact_for_relay,
        )),
    &session_id,
    allowlist.clone(),
);
```

`redact_for_relay` masks GitHub tokens, AWS/Google/Slack keys, bearer/JWT
credentials, `name=value` secret assignments, URL userinfo passwords, and
Signal device-link URIs (see `amplihack-signal`). A `ghp_…` GitHub token in a
failing turn, for example, is replaced with `[REDACTED-GITHUB-TOKEN]` in both
the surfaced error and the debug output.

---

## Configuration: `AMPLIHACK_TURN_ERROR_TAIL_BYTES`

The tail size is an explicit operator policy, not a fixed hardcoded cap.

| | |
| --- | --- |
| **Environment variable** | `AMPLIHACK_TURN_ERROR_TAIL_BYTES` |
| **Meaning** | Maximum number of trailing bytes of a failed turn's combined output to include in the surfaced error string. |
| **Type** | Unsigned integer (bytes). |
| **Default** | `2048` (the `DEFAULT_TURN_ERROR_TAIL_BYTES` constant) when the variable is unset. |
| **Value `0`** | Honored literally — the error carries no tail, only the exit-status summary. |
| **Unparseable value** | Any value that does not parse as an unsigned integer — including negative numbers and values that overflow `usize` — falls back to the default **and** emits a `tracing::warn!` naming the bad value. Misconfiguration is never silent. |

Raise the budget when you want more failure context inline; lower it (or set it
to `0`) to further reduce the amount of child output that can reach logs and
relays. There is no upper limit imposed by the driver — the value you set is
the value used — so choose it against your own log-hygiene policy.

### Examples

```bash
# Default behavior: up to 2048 bytes of tail in the error.
unset AMPLIHACK_TURN_ERROR_TAIL_BYTES

# Tighten to a 256-byte tail.
export AMPLIHACK_TURN_ERROR_TAIL_BYTES=256

# Emit only the exit-status summary, no child output at all.
export AMPLIHACK_TURN_ERROR_TAIL_BYTES=0

# Bad value -> falls back to 2048 and logs a warning naming "banana".
export AMPLIHACK_TURN_ERROR_TAIL_BYTES=banana
```

---

## Retrieving the full output at debug level

The complete combined output is never discarded — it is emitted at
`tracing::debug!` with structured fields, so operators keep full diagnosability
without exposing that output in the default error surface.

The debug event carries:

| Field | Meaning |
| --- | --- |
| `status` | The child's exit status. |
| `stdout_len` | Length of the captured stdout, in bytes. |
| `stderr_len` | Length of the captured stderr, in bytes. |
| `output` | The full combined stdout+stderr text, after redaction if a redactor is injected. |

Message: `copilot turn failed; full combined output at debug`.

> **Redaction note.** When a redactor is injected (see
> [The injected redactor](#the-injected-redactor)), the `output` field is the
> **redacted** combined output — the same scrubbed text used for the error tail.
> Without an injected redactor the field carries the raw combined output.

Enable it by configuring your `tracing` subscriber to allow `DEBUG` for the
`amplihack_turn` target, for example:

```bash
# With tracing-subscriber's EnvFilter:
export RUST_LOG=amplihack_turn=debug
```

> **Operational note.** The full `output` field is intended for opt-in
> diagnosis. Do not run production relays at `DEBUG` for `amplihack_turn` as a
> default, since that reintroduces the full child output into your log sink.

---

## API reference

Public surface (module `amplihack_turn::turn`):

### `DEFAULT_TURN_ERROR_TAIL_BYTES`

```rust
/// Default number of trailing bytes of a failed turn's combined stdout+stderr
/// to include in the surfaced error when `AMPLIHACK_TURN_ERROR_TAIL_BYTES` is
/// unset or cannot be parsed as an unsigned integer.
pub const DEFAULT_TURN_ERROR_TAIL_BYTES: usize = 2048;
```

The documented default budget. Exposed so callers and tests can reference the
same value the runner uses.

### `CopilotTurnRunner` (behavioral contract)

`CopilotTurnRunner` implements the `TurnRunner` trait. Its `run_argv` future
resolves to:

- `Ok(String)` — the child's full captured stdout, on a zero exit.
- `Err(io::Error)` — on a non-zero exit, an `io::Error::other(..)` whose
  message is `copilot turn failed ({status}); last {n} bytes of output: {tail}`
  as described in [What the error contains](#what-the-error-contains). Other
  error kinds (e.g. `Interrupted` for a pre-empted turn) are unaffected by this
  feature.

The tail budget and the char-boundary snapping are internal helpers; only the
default constant, the environment variable, and the `with_redactor` seam are
part of the public contract.

### `CopilotTurnRunner::new`

```rust
#[must_use]
pub fn new(program: impl Into<String>, preempt: PreemptSlot) -> Self
```

Construct a runner for `program` (typically `copilot`) sharing `preempt`. The
redactor is left unset (`None`), so the failure path is an exact no-op. The
signature is unchanged from before the redactor seam was added.

### `CopilotTurnRunner::with_redactor`

```rust
#[must_use]
pub fn with_redactor(
    mut self,
    redactor: std::sync::Arc<dyn Fn(&str) -> String + Send + Sync>,
) -> Self
```

Attach a redactor closure that is applied **once** to a failed turn's combined
output before it is used for the debug `output` field and the bounded error
tail. Builder style: consumes and returns `self`. The `Arc<dyn Fn + Send + Sync>`
type is required so the closure can cross the `Send` future's `await` boundary.
The closure must be total (return a `String`, never panic). Calling
`with_redactor` more than once keeps the last redactor supplied.

The production relay path injects
`amplihack_signal::chat::outbound::redact_for_relay`; see
[Production wiring](#production-wiring).

---

## Examples

### Reading a failed-turn error in the chat layer

The Signal chat layer surfaces the error unchanged apart from a prefix:

```text
turn failed: copilot turn failed (exit status: 2); last 137 bytes of output: ...trailing output...
```

Because the `copilot turn failed` prefix is preserved, any code that matches on
it (including the existing integration test asserting
`msg.contains("copilot turn failed")`) continues to work.

### Diagnosing a failure with full output

```bash
# Reproduce the failure with full child output visible.
RUST_LOG=amplihack_turn=debug amplihack signal chat ...

# In the logs, find:
#   DEBUG amplihack_turn: copilot turn failed; full combined output at debug
#       status=exit status: 2 stdout_len=41231 stderr_len=88 output=<full text>
```

When the chat relay redactor is wired, any `output` and error tail shown above
is already scrubbed — e.g. a leaked `ghp_…` token appears as
`[REDACTED-GITHUB-TOKEN]`.

### Injecting a custom redactor in an embedding

Callers embedding `CopilotTurnRunner` directly can supply any `Fn(&str) -> String`:

```rust
use std::sync::Arc;
use amplihack_turn::turn::CopilotTurnRunner;

// Mask an application-specific secret shape.
let runner = CopilotTurnRunner::new("copilot", preempt.clone())
    .with_redactor(Arc::new(|s: &str| s.replace("INTERNAL_TOKEN", "<redacted>")));
```

With no `with_redactor` call the failure path is unchanged (identity no-op).

---

## Design notes

- **Only the failure branch changed.** The success path still returns the full
  captured stdout as the turn output. Nothing about a successful turn's output
  is bounded or truncated.
- **Forward char-boundary snap.** Taking the last `budget` bytes can land in the
  middle of a multibyte UTF-8 sequence. Snapping the start index forward to the
  next boundary guarantees a valid `&str` slice and keeps the result within the
  budget. This mirrors the char-boundary-safe truncation used elsewhere for
  prompt injection handling.
- **No heavy dependencies.** The only new dependency is the lightweight
  `tracing` logging facade (no networking, negligible transitive cost). The
  `amplihack-turn` crate is always compiled and intentionally lean, so no
  regex engine, redactor, or `tokio` `net` feature was added. Redaction is an
  **injected closure**, not a dependency: the real regex-based redactor lives in
  `amplihack-signal` and is passed in at the production construction site, so
  `amplihack-turn`'s `Cargo.toml` gains nothing.
- **Redact via an injection seam, not a shared crate.** Rather than extract a
  shared redaction crate, the runner accepts an `Arc<dyn Fn(&str) -> String +
  Send + Sync>`. This keeps the lean crate dependency-free while still allowing
  the relay path to enforce redaction for defense-in-depth.
- **Redact once, before bounding.** The failure path redacts the combined output
  exactly once and then derives both the debug `output` field and the bounded
  tail from that single scrubbed string, so a secret cannot leak by straddling
  the tail cut.
- **No silent fallbacks.** An unparseable budget falls back to the default and
  warns; it is never quietly ignored.

---

## Security invariants

- **Bounded by default.** With no configuration, at most
  `DEFAULT_TURN_ERROR_TAIL_BYTES` (2048) bytes of child output can appear in the
  surfaced/relayed error. Full output is never embedded in full by default.
- **Operator-gated full output.** The complete stdout+stderr is available only
  at `tracing::debug!`, which is opt-in.
- **Panic-free on hostile input.** Lossy UTF-8 decode plus a forward
  char-boundary snap mean arbitrary child bytes cannot panic the tail
  computation.
- **Log-flood resistance.** The error-message size is independent of child
  output size, so a runaway child cannot inflate logs or relayed messages
  through this path.
- **Stable prefix.** `copilot turn failed ({status})` is preserved for
  downstream parsing and for the chat relay message.
- **Defense-in-depth redaction.** When a redactor is injected, secrets matching
  its patterns are masked in **both** the surfaced error tail and the debug
  `output` field, with a single pass that runs **before** the tail is bounded.
  The production relay path wires `redact_for_relay`, so a leaked token in a
  failing turn does not reach logs or the relay in the clear.
- **No-op default is safe by construction.** With no redactor injected the
  failure path neither allocates nor calls anything, so the always-compiled
  `amplihack-turn` crate carries no regex or redaction dependency. Redaction is
  a property of the relay wiring, not of the lean crate.
- **Residual risk.** The relay redactor covers known secret shapes (GitHub, AWS,
  Google, Slack, bearer/JWT, `name=value` assignments, URL userinfo, Signal
  device links). Non-matching secret formats are not masked; that residual risk
  is unchanged by this feature and is tracked separately from #1108.

---

## Testing

Validation gate:

```bash
cargo fmt --all
cargo clippy -p amplihack-turn --all-targets -- -D warnings
cargo test  -p amplihack-turn --test turn_error_it     # tail + redactor seam
cargo test  -p amplihack-turn --test agent_session_it  # prefix contract
cargo test  -p amplihack-signal --features signal --test chat_it  # real redactor
cargo build -p amplihack-turn
```

Coverage in `crates/amplihack-turn/tests/turn_error_it.rs`:

- **Multibyte safety.** A failing turn whose combined output ends in multibyte
  UTF-8 characters longer than the tail budget produces a bounded tail and does
  not panic.
- **Bound honored.** The returned error's tail portion is `<= budget` bytes
  (allowing for the forward char-boundary snap).
- **Env override respected.** Setting `AMPLIHACK_TURN_ERROR_TAIL_BYTES` to a
  small value shrinks the tail; an unparseable value falls back to
  `DEFAULT_TURN_ERROR_TAIL_BYTES`.
- **Prefix preserved.** The error still contains `copilot turn failed`.
- **Full output at debug only.** A captured `tracing` subscriber observes the
  full combined output at `DEBUG`, and that full output is **not** present in
  the returned error string when it exceeds the tail budget.
- **Redactor seam masks both sinks.** A runner built with a mock redactor
  (`Arc::new(|s| s.replace("SEAMSECRET_DoNotUse", "<redacted>"))`) over an
  `sh -c` program that prints the fake secret and exits non-zero yields a
  returned error that contains `<redacted>` and **not** `SEAMSECRET_DoNotUse`
  (still starting with `copilot turn failed`). A scoped `tracing_subscriber`
  with a `Mutex<Vec<u8>>`-backed writer confirms the captured `DEBUG` bytes
  contain `<redacted>` and not the raw secret.
- **No-op default proven.** The same fake secret run through a plain
  `CopilotTurnRunner::new` (no `with_redactor`) passes through **unredacted** in
  both sinks, proving the seam — not some hidden default — does the work.

Coverage in `crates/amplihack-signal/tests/chat_it.rs` (feature `signal`):

- **Real redactor wired end-to-end.** A runner built with
  `with_redactor(Arc::new(redact_for_relay))` over a program that emits an
  obviously-fake `ghp_…` token then exits non-zero surfaces an error that does
  **not** contain the raw token and **does** contain `[REDACTED-GITHUB-TOKEN]`,
  proving the production seam redacts real secret shapes.

> **Test hygiene.** Environment-mutating tests are serialized through a
> process-wide mutex and wrap `std::env::set_var`/`remove_var` in `unsafe`
> (edition 2024) to avoid cross-test races, matching the pattern used elsewhere
> in the workspace.

---

## See also

- [Signal Chat Hardening](signal-chat-hardening.md)
- [Signal Channel Turn Loop](signal-channel-turn-loop.md)
- [`crates/amplihack-turn`](../crates/amplihack-turn)
