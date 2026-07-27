//! Demolition guard for the dead automatic session-start Signal channel.
//!
//! Context: the always-on, per-session Signal group + background subscriber was
//! the ONLY code that ever set `SignalState.group_id`. Its creation was removed
//! in #1072, leaving every session-start channel function permanently dead:
//! they read a `group_id` that is now never written. This suite specifies the
//! *target* post-demolition source shape so the removal is complete and the
//! dead code can never silently creep back.
//!
//! This is a **source-text** guard (not a runtime behavior test): the removed
//! symbols cannot be referenced from Rust once deleted, so we assert against the
//! crate's own source files. It is intentionally NOT `#[cfg(feature = "signal")]`
//! — the dead code and its feature-off no-op shims must both be gone, so the
//! guard runs and must pass in **both** feature configurations.
//!
//! RED (pre-implementation): the channel code, state, shims, call sites, and the
//! `signal-subscriber` binary arm still exist, so every "must NOT contain"
//! assertion fails today. GREEN once the demolition lands.

use std::path::{Path, PathBuf};

/// Absolute path to the `amplihack-hooks` crate root (this test's crate).
fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

/// Read a source file relative to the crate root. Missing files are treated as
/// empty text so "must NOT contain" assertions naturally pass for deleted files
/// (a deleted module contains none of the banned symbols).
fn read_rel(rel: &str) -> String {
    let path = crate_root().join(rel);
    std::fs::read_to_string(path).unwrap_or_default()
}

/// The hooks *binary* lives in a sibling crate (`bins/amplihack-hooks`). Reach
/// it relative to this crate root so the guard can assert the `signal-subscriber`
/// dispatch arm is gone without depending on the binary crate's build.
fn bin_main() -> String {
    let path = crate_root().join("../../bins/amplihack-hooks/src/main.rs");
    std::fs::read_to_string(path).unwrap_or_default()
}

fn exists(rel: &str) -> bool {
    crate_root().join(rel).exists()
}

/// Assert `needle` is absent from `haystack`, with a descriptive failure.
#[track_caller]
fn assert_absent(haystack: &str, needle: &str, ctx: &str) {
    assert!(
        !haystack.contains(needle),
        "demolition incomplete: {ctx} must no longer reference `{needle}` \
         (dead session-start Signal channel)"
    );
}

/// Assert `needle` is present, guarding retained onboarding surface.
#[track_caller]
fn assert_present(haystack: &str, needle: &str, ctx: &str) {
    assert!(
        haystack.contains(needle),
        "regression: {ctx} must still contain `{needle}` (retained onboarding path)"
    );
}

// ---------------------------------------------------------------------------
// 1. Transitively-dead modules are deleted outright.
// ---------------------------------------------------------------------------

#[test]
fn dead_channel_modules_are_deleted() {
    assert!(
        !exists("src/signal_integration/host_context.rs"),
        "host_context.rs is reachable only from the removed `drain_into_context`; delete it"
    );
    assert!(
        !exists("src/signal_integration/outbound.rs"),
        "outbound.rs is reachable only from the removed `relay_outbound_inner`; delete it"
    );
}

// ---------------------------------------------------------------------------
// 2. imp.rs: every dead channel function/state/const/helper is gone.
// ---------------------------------------------------------------------------

#[test]
fn imp_drops_dead_channel_functions() {
    let imp = read_rel("src/signal_integration/imp.rs");
    for sym in [
        "fn relay_outbound", // covers relay_outbound + relay_outbound_inner
        "fn drain_into_context",
        "fn on_stop",
        "fn stop(", // private stop() teardown helper
        "fn run_subscriber",
        "fn subscriber_main",
        "fn stop_subscriber",
        "fn pid_is_our_subscriber",
        "fn record_connect_failure",
        "fn next_retry_delay",
        "fn runtime(", // tokio runtime builder
        "with_timeout",
        "fn state_path",
        "fn set_process_enabled",
        "fn process_enabled",
        "fn is_channel_configured",
        "fn format_operator_context", // only consumer was drain_into_context
        "verified_send",
        "expected_members",
    ] {
        assert_absent(&imp, sym, "signal_integration::imp");
    }
}

#[test]
fn imp_drops_dead_state_and_flags() {
    let imp = read_rel("src/signal_integration/imp.rs");
    for sym in [
        "struct SignalState",
        "group_id",
        "subscriber_pid",
        "SIGNAL_ENABLED",
        "AtomicJsonFile",
        "Gate", // inbound allowlist gate, used only by the removed subscriber
    ] {
        assert_absent(&imp, sym, "signal_integration::imp");
    }
}

// ---------------------------------------------------------------------------
// 3. imp.rs: the onboarding-only surface is retained verbatim.
// ---------------------------------------------------------------------------

#[test]
fn imp_retains_onboarding_surface() {
    let imp = read_rel("src/signal_integration/imp.rs");
    for sym in [
        "fn on_session_start",
        "fn start",
        "fn maybe_prompt_onboarding",
        "fn load_config_or_disabled",
        "fn normalize_session_id",
        "fn signal_root",
        "fn is_stderr_tty",
    ] {
        assert_present(&imp, sym, "signal_integration::imp");
    }
}

// ---------------------------------------------------------------------------
// 4. Facade (mod.rs): dead re-exports and feature-off shims removed; only the
//    onboarding entry points remain.
// ---------------------------------------------------------------------------

#[test]
fn facade_drops_dead_reexports_and_shims() {
    let facade = read_rel("src/signal_integration/mod.rs");
    for sym in [
        "run_subscriber",
        "drain_into_context",
        "on_stop",
        "relay_outbound",
        "set_process_enabled",
        "is_channel_configured",
        "inject_host",
        "merge_additional_context",
        "mod host_context",
        "mod outbound",
    ] {
        assert_absent(&facade, sym, "signal_integration facade (mod.rs)");
    }
}

#[test]
fn facade_retains_onboarding_entry_points() {
    let facade = read_rel("src/signal_integration/mod.rs");
    assert_present(&facade, "on_session_start", "signal_integration facade");
    assert_present(&facade, "mod onboarding", "signal_integration facade");
}

// ---------------------------------------------------------------------------
// 5. Hook call sites: every reference to a removed channel fn is deleted.
// ---------------------------------------------------------------------------

#[test]
fn user_prompt_drops_channel_call_sites() {
    let src = read_rel("src/user_prompt/mod.rs");
    for sym in [
        "relay_outbound",
        "drain_into_context",
        "is_channel_configured",
        "inject_host",
        "merge_additional_context",
    ] {
        assert_absent(&src, sym, "user_prompt::mod");
    }
}

#[test]
fn post_tool_use_drops_channel_call_sites() {
    let src = read_rel("src/post_tool_use/mod.rs");
    for sym in [
        "drain_into_context",
        "inject_host",
        "merge_additional_context",
    ] {
        assert_absent(&src, sym, "post_tool_use::mod");
    }
}

#[test]
fn session_stop_drops_on_stop_call_site() {
    let src = read_rel("src/session_stop/mod.rs");
    // Match the call form `on_stop(` so the legitimate `session_stop` module /
    // hook identifiers (which contain the substring "on_stop") are not flagged.
    assert_absent(&src, "on_stop(", "session_stop::mod");
}

#[test]
fn stop_drops_relay_and_outbound_transcript_machinery() {
    let src = read_rel("src/stop/mod.rs");
    for sym in [
        "relay_outbound",
        "read_transcript_tail_bounded",
        "last_assistant_message_from_transcript",
        "open_transcript_nonblocking",
        "extract_assistant_text_from_entry",
        "OUTBOUND_TRANSCRIPT_READ_CAP",
    ] {
        assert_absent(&src, sym, "stop::mod");
    }
}

// ---------------------------------------------------------------------------
// 6. Hooks binary: the `signal-subscriber` dispatch arm and process-enable call
//    are removed; the remaining subcommands stay intact.
// ---------------------------------------------------------------------------

#[test]
fn bin_drops_signal_subscriber_arm() {
    let main = bin_main();
    for sym in ["signal-subscriber", "run_subscriber", "set_process_enabled"] {
        assert_absent(&main, sym, "bins/amplihack-hooks main.rs");
    }
}

#[test]
fn bin_retains_core_subcommands() {
    let main = bin_main();
    for sym in [
        "pre-tool-use",
        "post-tool-use",
        "session-start",
        "user-prompt",
        "pre-compact",
        "precommit-prefs",
    ] {
        assert_present(&main, sym, "bins/amplihack-hooks main.rs");
    }
}

// ---------------------------------------------------------------------------
// 7. Obsolete channel integration tests are deleted.
// ---------------------------------------------------------------------------

#[test]
fn obsolete_channel_tests_are_deleted() {
    for rel in [
        "tests/signal_outbound_relay_it.rs",
        "tests/signal_host_aware_context_it.rs",
    ] {
        assert!(
            !exists(rel),
            "{rel} exercises removed channel behavior and must be deleted"
        );
    }
    let bin_sub = crate_root().join("../../bins/amplihack-hooks/tests/signal_subscriber_it.rs");
    assert!(
        !Path::new(&bin_sub).exists(),
        "bins/amplihack-hooks/tests/signal_subscriber_it.rs exercises the removed \
         subscriber subcommand and must be deleted"
    );
}

// ---------------------------------------------------------------------------
// 8. The retained onboarding integration test is preserved.
// ---------------------------------------------------------------------------

#[test]
fn onboarding_test_is_retained() {
    assert!(
        exists("tests/signal_onboarding_it.rs"),
        "the onboarding integration test must remain (retained behavior)"
    );
}
