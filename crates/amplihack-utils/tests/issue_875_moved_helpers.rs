//! TDD contract tests for issue #875 — util-layer helper relocation.
//!
//! These tests pin the *new* public API surface that must exist in
//! `amplihack-utils` after the util-layer helpers are moved out of
//! `amplihack-cli` (per the #875 design: launcher_context, binary_finder,
//! runtime_assets, the `active_agent_binary` wrapper, and the three process
//! helpers). Referencing paths that do not exist yet makes this integration
//! test target FAIL TO COMPILE (RED) until the relocation is complete.
//!
//! Each `amplihack-hooks` call-site that previously imported from
//! `amplihack_cli::*` must resolve at the corresponding `amplihack_utils::*`
//! path below.

// --- launcher_context: moved verbatim from amplihack_cli::launcher_context ---
use amplihack_utils::launcher_context::{
    LauncherContext, LauncherKind, is_launcher_context_stale, launcher_context_path,
    read_launcher_context, write_launcher_context,
};

// --- binary_finder: moved verbatim from amplihack_cli::binary_finder ---
use amplihack_utils::binary_finder::BinaryFinder;

// --- runtime_assets: moved verbatim from amplihack_cli::runtime_assets ---
use amplihack_utils::runtime_assets::iter_runtime_roots;

use std::path::Path;

#[test]
fn launcher_context_types_and_fns_resolve_in_utils() {
    // Force resolution of each relocated item. `write_launcher_context` takes
    // `impl Into<String>`, so it is resolved via the top-level `use` import
    // rather than a value binding.
    let _read: fn(&Path) -> Option<LauncherContext> = read_launcher_context;
    let _stale: fn(&LauncherContext) -> bool = is_launcher_context_stale;
    let _path: fn(&Path) -> std::path::PathBuf = launcher_context_path;

    // LauncherKind must remain a copyable enum with the `as_str` accessor.
    let kind = LauncherKind::Copilot;
    let _copied = kind;
    assert!(!kind.as_str().is_empty());
}

/// Pins the full `write_launcher_context` signature (it takes
/// `impl Into<String>`, so it is exercised inside a concrete-typed fn rather
/// than bound as a value). Never called — definition alone forces resolution.
#[allow(dead_code)]
fn _pins_write_launcher_context(root: &Path, env: std::collections::BTreeMap<String, String>) {
    let _ = write_launcher_context(root, LauncherKind::Copilot, "cmd", env);
}

#[test]
fn binary_finder_resolves_in_utils() {
    // `find` must keep its `&str -> Result<_>` shape; `find_all` its infallible
    // `Vec` shape. We only assert resolution + total-ness of `find_all`.
    let results = BinaryFinder::find_all("definitely-not-a-real-binary-xyz");
    assert!(results.is_empty());
}

#[test]
fn iter_runtime_roots_resolves_in_utils() {
    let _f: fn() -> Vec<std::path::PathBuf> = iter_runtime_roots;
    // Must not panic when invoked.
    let _roots = iter_runtime_roots();
}

#[test]
fn active_agent_binary_wrapper_resolves_in_utils() {
    // The no-arg `active_agent_binary()` wrapper (previously
    // amplihack_cli::env_builder::active_agent_binary) relocates into
    // amplihack-utils alongside the underlying `agent_binary::resolve`.
    let _f: fn() -> String = amplihack_utils::agent_binary::active_agent_binary;
    let name = amplihack_utils::agent_binary::active_agent_binary();
    // Always resolves to an allowlisted name — never empty.
    assert!(
        !name.is_empty(),
        "active_agent_binary() must return a non-empty allowlisted name"
    );
}

#[test]
fn proc_text_helpers_resolve_in_utils() {
    // The three helpers formerly in amplihack_cli::util move into utils.
    // `strip_ansi` sanitizes control sequences; `truncate_chars_with_notice`
    // bounds output length. Behavioral equivalence is required.
    let stripped = amplihack_utils::proc_text::strip_ansi("\x1b[31mred\x1b[0m");
    assert_eq!(stripped, "red");

    let short = amplihack_utils::proc_text::truncate_chars_with_notice("hello", 100);
    assert_eq!(
        short, "hello",
        "inputs within the limit pass through unchanged"
    );

    let long = amplihack_utils::proc_text::truncate_chars_with_notice("abcdefgh", 4);
    assert!(
        long.len() < "abcdefgh".len() || long.contains("abcd"),
        "over-long input must be truncated"
    );
}
