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

/// Number of most-recent outbound fingerprints considered "recent" for
/// echo-suppression. Matching is restricted to this trailing window so that a
/// short operator instruction (e.g. "continue") that merely coincides with a
/// long-past mirrored line is still delivered rather than silently dropped.
/// The on-disk log is also trimmed toward this size to bound growth.
const FP_WINDOW: usize = 128;

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
///
/// The log is kept bounded to the most recent [`FP_WINDOW`] fingerprints: after
/// appending, an over-long file is rewritten to retain only the tail. This caps
/// both on-disk growth and the per-inbound scan cost over a long session.
pub fn record_outbound_fingerprint(root: &Path, session_id: &str, body: &str) -> io::Result<()> {
    let path = session_fp_path(root, session_id);
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    {
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)?;
        writeln!(file, "{}", hash_hex(body))?;
    }
    // Bound the file to the most recent FP_WINDOW entries. Only rewrite when it
    // has grown past a hysteresis threshold to avoid rewriting on every call.
    if let Ok(content) = std::fs::read_to_string(&path) {
        let lines: Vec<&str> = content.lines().collect();
        if lines.len() > FP_WINDOW * 2 {
            let tail = lines[lines.len() - FP_WINDOW..].join("\n");
            std::fs::write(&path, format!("{tail}\n"))?;
        }
    }
    Ok(())
}

/// Whether `body` was recently mirrored outbound for `session_id` under `root`
/// (i.e. its fingerprint appears within the most recent [`FP_WINDOW`] mirrored
/// entries). Fingerprints are isolated per session.
///
/// Matching is deliberately restricted to a recent window rather than the whole
/// session history: an inbound operator message whose text happens to equal a
/// long-past mirrored line (realistic for short instructions like "continue" or
/// "yes") must still reach the CLI rather than being suppressed as a self-echo.
#[must_use]
pub fn is_recent_outbound_fingerprint(root: &Path, session_id: &str, body: &str) -> bool {
    let path = session_fp_path(root, session_id);
    let Ok(content) = std::fs::read_to_string(&path) else {
        return false;
    };
    let fp = hash_hex(body);
    let lines: Vec<&str> = content.lines().collect();
    let start = lines.len().saturating_sub(FP_WINDOW);
    lines[start..].iter().any(|line| line.trim() == fp)
}

/// Remove the persisted outbound-fingerprint log for `session_id` under `root`.
/// Called during per-session teardown so the log does not outlive the session.
pub fn clear_outbound_fingerprints(root: &Path, session_id: &str) {
    let path = session_fp_path(root, session_id);
    let _ = std::fs::remove_file(path);
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::TempDir;

    #[test]
    fn recent_fingerprint_matches_within_window() {
        let td = TempDir::new().unwrap();
        record_outbound_fingerprint(td.path(), "s", "hello").unwrap();
        assert!(is_recent_outbound_fingerprint(td.path(), "s", "hello"));
        assert!(!is_recent_outbound_fingerprint(td.path(), "s", "other"));
    }

    #[test]
    fn old_fingerprint_outside_window_is_not_recent() {
        // A short operator instruction that coincides with a long-past mirrored
        // line must still be delivered (not suppressed as an echo).
        let td = TempDir::new().unwrap();
        record_outbound_fingerprint(td.path(), "s", "continue").unwrap();
        for i in 0..(FP_WINDOW * 2) {
            record_outbound_fingerprint(td.path(), "s", &format!("line-{i}")).unwrap();
        }
        assert!(
            !is_recent_outbound_fingerprint(td.path(), "s", "continue"),
            "a fingerprint older than FP_WINDOW must not count as recent"
        );
        // The most recent entry is still recognized.
        let last = format!("line-{}", FP_WINDOW * 2 - 1);
        assert!(is_recent_outbound_fingerprint(td.path(), "s", &last));
    }

    #[test]
    fn log_is_bounded_in_size() {
        let td = TempDir::new().unwrap();
        for i in 0..(FP_WINDOW * 4) {
            record_outbound_fingerprint(td.path(), "s", &format!("m-{i}")).unwrap();
        }
        let content = std::fs::read_to_string(session_fp_path(td.path(), "s")).unwrap();
        let lines = content.lines().count();
        assert!(
            lines <= FP_WINDOW * 2,
            "fingerprint log must stay bounded, got {lines} lines"
        );
    }

    #[test]
    fn clear_removes_fingerprint_log() {
        let td = TempDir::new().unwrap();
        record_outbound_fingerprint(td.path(), "s", "x").unwrap();
        assert!(is_recent_outbound_fingerprint(td.path(), "s", "x"));
        clear_outbound_fingerprints(td.path(), "s");
        assert!(!is_recent_outbound_fingerprint(td.path(), "s", "x"));
    }
}
