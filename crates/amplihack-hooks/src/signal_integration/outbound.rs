//! R4 — full-conversation mirroring helpers: outbound size bounding + a
//! cross-process echo-suppression fingerprint.
//!
//! Mirroring every user prompt and assistant turn to the Signal group raises two
//! hazards:
//!
//! 1. **Unbounded size.** Assistant turns can be huge. [`truncate_for_relay`]
//!    bounds each mirrored body to [`RELAY_MAX_BYTES`] at a UTF-8 char boundary
//!    (never splitting a multibyte code point) and appends a visible truncation
//!    marker.
//! 2. **Echo loops across processes.** The outbound mirror runs in the (short-
//!    lived) hook process while the inbound subscriber runs detached, so the
//!    in-memory echo window in `amplihack_signal::gating::Gate` cannot span the
//!    two. [`record_outbound_fingerprint`] persists a hashed, per-session
//!    fingerprint of each mirrored body that the subscriber checks via
//!    [`is_recent_outbound_fingerprint`] to drop the account's own synced-back
//!    messages instead of re-injecting them.
//!
//! Both fingerprint seams take an explicit `root`, keeping them hermetic.

use std::io::{self, Write};
use std::path::{Path, PathBuf};

use sha2::{Digest, Sha256};

/// Maximum bytes of a single mirrored message body (4 KiB).
pub const RELAY_MAX_BYTES: usize = 4096;

/// Marker appended to a truncated body. Placed so the total length up to the
/// word `truncated` never exceeds the byte cap (the cap reserves room for it).
const TRUNCATION_MARKER: &str = " [truncated]";

/// Bound `body` to at most `max` bytes at a UTF-8 char boundary, appending a
/// visible truncation marker when shortened. Short or empty bodies are returned
/// unchanged.
#[must_use]
pub fn truncate_for_relay(body: &str, max: usize) -> String {
    if body.len() <= max {
        return body.to_string();
    }
    // Reserve room for the marker so the mirrored prefix (including the marker
    // text preceding "truncated") never exceeds `max`.
    let budget = max.saturating_sub(TRUNCATION_MARKER.len());
    let mut end = budget.min(body.len());
    while end > 0 && !body.is_char_boundary(end) {
        end -= 1;
    }
    let mut out = String::with_capacity(end + TRUNCATION_MARKER.len());
    out.push_str(&body[..end]);
    out.push_str(TRUNCATION_MARKER);
    out
}

/// Lowercase hex SHA-256 of `s`.
fn hash_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    format!("{:x}", hasher.finalize())
}

/// Per-session fingerprint log path under `root` (session id is hashed into the
/// filename so it is filesystem-safe and cannot escape `root`).
fn session_fp_path(root: &Path, session_id: &str) -> PathBuf {
    root.join("signal-outbound-fp").join(hash_hex(session_id))
}

/// Persist a fingerprint of an outbound (mirrored) `body` for `session_id` under
/// `root`, so a detached subscriber can later recognize the echo.
pub fn record_outbound_fingerprint(root: &Path, session_id: &str, body: &str) -> io::Result<()> {
    let path = session_fp_path(root, session_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let mut file = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)?;
    writeln!(file, "{}", hash_hex(body))
}

/// Whether `body` was recently mirrored outbound for `session_id` under `root`
/// (i.e. its fingerprint is present). Fingerprints are isolated per session.
#[must_use]
pub fn is_recent_outbound_fingerprint(root: &Path, session_id: &str, body: &str) -> bool {
    let path = session_fp_path(root, session_id);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };
    let fp = hash_hex(body);
    content.lines().any(|line| line.trim() == fp)
}
