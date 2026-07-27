//! Feature-gated Signal channel integration for the hook lifecycle.
//!
//! This module is the seam between the host-agnostic hooks and the
//! [`amplihack_signal`] crate. **The entire feature is gated on the `signal`
//! cargo feature (default OFF).** With the feature off every entry point below
//! is a zero-cost no-op shim, so the standard hook binary carries no Signal
//! code and no `tokio` net stack.
//!
//! # Lifecycle
//!
//! - [`on_session_start`] — **no Signal group I/O.** Session start never creates
//!   a group, posts a "session started" message, persists a group id, or spawns
//!   a subscriber (that always-on behavior flooded the operator with empty
//!   groups). Signal groups are created only by the explicit `amplihack signal
//!   chat <topic>` command. The only remaining behavior is a one-time,
//!   purely-local onboarding notice on an unconfigured interactive host.
//! - [`drain_into_context`] — drain the file-backed inbox of operator
//!   instructions so `PostToolUse` / `UserPromptSubmit` can surface them as
//!   `additionalContext`.
//! - [`on_stop`] — post a session summary, `quitGroup`, and stop the subscriber.
//! - [`run_subscriber`] — the long-lived inbound subscriber (the entry point of
//!   the `amplihack-hooks signal-subscriber` subcommand).
//!
//! # Trust boundary
//!
//! Inbound Signal text is **advisory data, never commands**. It is only ever
//! surfaced to the agent as `additionalContext`; nothing here executes it. The
//! gate is fail-closed (allowlist + `device == 1` + `groupId` match + bounded
//! echo suppression), and the account's own synced-back messages are dropped.
//!
//! # Failure policy
//!
//! Every operation is **non-fatal**: failures are logged via `tracing` and (for
//! `SessionStart`) appended to the hook `warnings[]`, but never abort a hook.

#[cfg(feature = "signal")]
mod imp;

#[cfg(feature = "signal")]
mod host_context;
/// R1 — Signal onboarding prompt gating + declined sentinel.
#[cfg(feature = "signal")]
pub mod onboarding;
/// R4 — full-conversation mirroring helpers (outbound truncation + echo
/// fingerprints).
#[cfg(feature = "signal")]
pub mod outbound;

#[cfg(feature = "signal")]
pub use host_context::{inject_host, merge_additional_context};

#[cfg(feature = "signal")]
pub use imp::run_subscriber;

#[cfg(feature = "signal")]
pub use imp::{
    drain_into_context, is_channel_configured, on_session_start, on_stop, relay_outbound,
    set_process_enabled,
};

// ---------------------------------------------------------------------------
// No-op shims (feature OFF). Signatures mirror the real implementation so the
// hook seams compile and link identically regardless of the feature.
// ---------------------------------------------------------------------------

/// Session-start Signal hook. Performs no Signal group I/O (no group creation,
/// no "session started" post, no subscriber spawn). No-op when the `signal`
/// feature is disabled.
#[cfg(not(feature = "signal"))]
pub fn on_session_start(_session_id: Option<&str>, _warnings: &mut Vec<String>) {}

/// Drain queued operator instructions for injection as `additionalContext`.
/// Always `None` when the `signal` feature is disabled.
#[cfg(not(feature = "signal"))]
#[must_use]
pub fn drain_into_context(_session_id: Option<&str>) -> Option<String> {
    None
}

/// Post a session summary, leave the group, and stop the subscriber. No-op
/// when the `signal` feature is disabled.
#[cfg(not(feature = "signal"))]
pub fn on_stop(_session_id: &str) {}

/// Mirror an outbound line (user prompt / assistant turn) to the session group.
/// No-op when the `signal` feature is disabled.
#[cfg(not(feature = "signal"))]
pub fn relay_outbound(_session_id: Option<&str>, _body: &str) {}

/// Enable/disable real Signal I/O for this process. No-op when the `signal`
/// feature is disabled.
#[cfg(not(feature = "signal"))]
pub fn set_process_enabled(_enabled: bool) {}

/// Whether the Signal channel is configured/usable. Always `false` when the
/// `signal` feature is disabled, so hook output keeps its historical shape.
#[cfg(not(feature = "signal"))]
#[must_use]
pub fn is_channel_configured() -> bool {
    false
}
