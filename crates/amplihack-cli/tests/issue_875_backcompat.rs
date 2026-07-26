//! TDD contract tests for issue #875 — CLI back-compat re-exports.
//!
//! The #875 design keeps the `amplihack_cli::*` public API *unchanged* by
//! converting every relocated module into a `pub use` re-export pointing at its
//! new home in `amplihack-utils` / `amplihack-memory`. This regression guard
//! pins that public surface: it passes today and MUST keep passing after the
//! refactor, catching any accidental removal of the re-exports.

#[allow(unused_imports)]
use amplihack_cli::binary_finder::BinaryFinder;
#[allow(unused_imports)]
use amplihack_cli::env_builder::active_agent_binary;
#[allow(unused_imports)]
use amplihack_cli::launcher_context::{
    LauncherContext, LauncherKind, is_launcher_context_stale, launcher_context_path,
    read_launcher_context, write_launcher_context,
};
#[allow(unused_imports)]
use amplihack_cli::memory::{
    IndexStatus, PromptContextMemory, background_index_job_active, check_index_status,
    default_code_graph_db_path_for_project, record_background_index_pid,
    resolve_code_graph_db_path_for_project, retrieve_prompt_context_memories,
    store_session_learning, summarize_code_graph,
};
#[allow(unused_imports)]
use amplihack_cli::runtime_assets::iter_runtime_roots;

#[test]
fn cli_util_layer_reexports_remain_public() {
    let _finder = BinaryFinder::find_all("definitely-not-a-real-binary-xyz");
    let _roots: Vec<std::path::PathBuf> = iter_runtime_roots();
    let _kind = LauncherKind::Copilot;
    let _abin: fn() -> String = active_agent_binary;
    let _r = read_launcher_context;
    let _s = is_launcher_context_stale;
    let _p = launcher_context_path;
    // Silence "unused" for the type-only import.
    fn _accepts_ctx(_c: &LauncherContext) {}
}

/// Pins the full `write_launcher_context` re-export signature (takes
/// `impl Into<String>`). Never called — definition alone forces resolution.
#[allow(dead_code)]
fn _pins_write_launcher_context(
    root: &std::path::Path,
    env: std::collections::BTreeMap<String, String>,
) {
    let _ = write_launcher_context(root, LauncherKind::Copilot, "cmd", env);
}

#[test]
fn cli_memory_facade_reexports_remain_public() {
    let _f0 = background_index_job_active;
    let _f1 = check_index_status;
    let _f2 = default_code_graph_db_path_for_project;
    let _f3 = record_background_index_pid;
    let _f4 = resolve_code_graph_db_path_for_project;
    let _f5 = retrieve_prompt_context_memories;
    let _f6 = store_session_learning;
    let _f7 = summarize_code_graph;
    fn _accepts_status(_s: &IndexStatus) {}
    fn _accepts_prompt_ctx(_m: &PromptContextMemory) {}
}

#[test]
fn cli_code_graph_import_reexport_remains_public() {
    let _f = amplihack_cli::commands::memory::code_graph::import_blarify_json;
}
