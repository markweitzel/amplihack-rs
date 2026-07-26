//! TDD contract tests for issue #875 — memory / code-graph helper relocation.
//!
//! These tests pin the *new* public API surface that must exist in
//! `amplihack-memory` after the code-graph and prompt-context helpers are
//! moved out of `amplihack-cli` (per the #875 design: a `memory` facade
//! mirroring the old `amplihack_cli::memory` facade, plus the code-graph
//! closure under a namespaced `code_index` module).
//!
//! Referencing paths that do not exist yet makes this integration test target
//! FAIL TO COMPILE (RED) until the relocation is complete. Every
//! `amplihack-hooks` call-site that previously imported from
//! `amplihack_cli::memory::*` or
//! `amplihack_cli::commands::memory::code_graph::*` must resolve at the
//! corresponding `amplihack_memory::*` path below.

// --- memory facade: the exact symbols hooks consumes today ---
#[allow(unused_imports)]
use amplihack_memory::memory::{
    IndexStatus, PromptContextMemory, background_index_job_active, check_index_status,
    default_code_graph_db_path_for_project, record_background_index_pid,
    resolve_code_graph_db_path_for_project, retrieve_prompt_context_memories,
    store_session_learning, summarize_code_graph,
};

use std::path::Path;

#[test]
fn memory_facade_functions_resolve_in_amplihack_memory() {
    // Force path resolution of each relocated free function via fn-item values.
    // If any path is missing, this target fails to compile (the RED state).
    let _f0 = background_index_job_active;
    let _f1 = check_index_status;
    let _f2 = default_code_graph_db_path_for_project;
    let _f3 = record_background_index_pid;
    let _f4 = resolve_code_graph_db_path_for_project;
    let _f5 = retrieve_prompt_context_memories;
    let _f6 = store_session_learning;
    let _f7 = summarize_code_graph;
}

#[test]
fn default_code_graph_db_path_is_deterministic_and_under_project() {
    // Behavioral contract preserved across the move: the default DB path is a
    // pure function of the project root and lives beneath it.
    let root = Path::new("/tmp/amplihack-issue-875-project");
    let path = default_code_graph_db_path_for_project(root)
        .expect("default code-graph DB path resolves for a project root");
    assert!(
        path.starts_with(root),
        "default code-graph DB path {:?} must live under the project root {:?}",
        path,
        root
    );
    // Determinism: same input -> same output.
    let again = default_code_graph_db_path_for_project(root)
        .expect("default code-graph DB path resolves for a project root");
    assert_eq!(path, again);
}

#[test]
fn index_status_and_prompt_context_types_resolve() {
    // Pin the two public types hooks passes around by reference / value.
    fn _accepts_status(_s: &IndexStatus) {}
    fn _accepts_prompt_ctx(_m: &PromptContextMemory) {}
}

#[test]
fn import_blarify_json_resolves_under_code_index() {
    // The blarify importer (used by a hooks test) relocates with the code-graph
    // subtree into `amplihack_memory::code_index::code_graph`.
    let _f = amplihack_memory::code_index::code_graph::import_blarify_json;
}
