//! Feature-gated Signal onboarding integration for the hook lifecycle.
//!
//! This module is the seam between the host-agnostic hooks and the
//! [`amplihack_signal`] crate. **The entire feature is gated on the `signal`
//! cargo feature (default OFF).** With the feature off the single entry point
//! below is a zero-cost no-op shim, so the standard hook binary carries no
//! Signal code.
//!
//! # Responsibility
//!
//! Session start performs **no Signal group I/O** — it never creates a group,
//! posts a message, persists any per-session state, or spawns a background
//! process. (That always-on behavior flooded the operator with empty groups.)
//! Signal groups are created only by the explicit `amplihack signal chat
//! <topic>` command, which lives entirely in the `amplihack-signal` crate and
//! the `amplihack signal chat` CLI command and is not touched here.
//!
//! The only behavior retained here is [`on_session_start`]: a one-time,
//! purely-local onboarding notice on an unconfigured interactive host.
//!
//! # Failure policy
//!
//! The operation is **non-fatal**: failures are logged via `tracing` and (for
//! `SessionStart`) appended to the hook `warnings[]`, but never abort a hook.

#[cfg(feature = "signal")]
mod imp;

/// R1 — Signal onboarding prompt gating + declined sentinel.
#[cfg(feature = "signal")]
pub mod onboarding;

#[cfg(feature = "signal")]
pub use imp::on_session_start;

// ---------------------------------------------------------------------------
// No-op shim (feature OFF). The signature mirrors the real implementation so
// the hook seam compiles and links identically regardless of the feature.
// ---------------------------------------------------------------------------

/// Session-start Signal hook. Performs no Signal group I/O. No-op when the
/// `signal` feature is disabled.
#[cfg(not(feature = "signal"))]
pub fn on_session_start(_session_id: Option<&str>, _warnings: &mut Vec<String>) {}
