//! TDD contract tests for issue #875.
//!
//! These tests assert the *core acceptance criterion*: the
//! `amplihack-hooks -> amplihack-cli` dependency edge is gone. They inspect
//! this crate's `Cargo.toml` manifest and its `src/` tree directly, so they
//! compile regardless of the refactor state and fail (RED) until the edge is
//! actually removed and the helper closure has been relocated to lower-level
//! crates (`amplihack-utils` / `amplihack-memory`).
//!
//! Expected lifecycle:
//!   * Before the refactor: every assertion below fails.
//!   * After the refactor: every assertion below passes.

use std::fs;
use std::path::{Path, PathBuf};

/// Root of the `amplihack-hooks` crate (the directory holding its Cargo.toml).
fn crate_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn read_manifest() -> String {
    let manifest = crate_root().join("Cargo.toml");
    fs::read_to_string(&manifest)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", manifest.display()))
}

/// Returns true if `line`, once stripped of a trailing `# comment`, declares a
/// Cargo dependency whose *key* is exactly `dep_name` (e.g. `amplihack-cli`).
///
/// Matches both `amplihack-cli = { ... }` and `amplihack-cli = "1"` forms while
/// ignoring keys that merely start with the same prefix (e.g.
/// `amplihack-cli-extras`).
fn line_declares_dep(line: &str, dep_name: &str) -> bool {
    let code = line.split('#').next().unwrap_or("").trim();
    match code.split_once('=') {
        Some((key, _)) => key.trim() == dep_name,
        None => false,
    }
}

/// Recursively collect every `*.rs` file under `dir`.
fn collect_rs_files(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_rs_files(&path, out);
        } else if path.extension().and_then(|e| e.to_str()) == Some("rs") {
            out.push(path);
        }
    }
}

#[test]
fn cargo_manifest_does_not_depend_on_amplihack_cli() {
    let manifest = read_manifest();
    let offending: Vec<&str> = manifest
        .lines()
        .filter(|line| line_declares_dep(line, "amplihack-cli"))
        .collect();

    assert!(
        offending.is_empty(),
        "issue #875: crates/amplihack-hooks/Cargo.toml must NOT declare an \
         `amplihack-cli` dependency, but found:\n{}",
        offending.join("\n")
    );
}

#[test]
fn cargo_manifest_depends_on_amplihack_memory() {
    // The relocated code-graph / memory helpers live in amplihack-memory after
    // the refactor, so hooks must gain this lower-level dependency.
    let manifest = read_manifest();
    let has_memory = manifest
        .lines()
        .any(|line| line_declares_dep(line, "amplihack-memory"));

    assert!(
        has_memory,
        "issue #875: crates/amplihack-hooks/Cargo.toml must declare an \
         `amplihack-memory` dependency (destination of the relocated memory \
         helpers)."
    );
}

#[test]
fn cargo_manifest_still_depends_on_amplihack_utils() {
    // Util-layer helpers (launcher_context, binary_finder, runtime_assets,
    // active_agent_binary) move into amplihack-utils, which hooks already
    // depends on. Guard against accidental removal.
    let manifest = read_manifest();
    let has_utils = manifest
        .lines()
        .any(|line| line_declares_dep(line, "amplihack-utils"));

    assert!(
        has_utils,
        "issue #875: crates/amplihack-hooks/Cargo.toml must retain its \
         `amplihack-utils` dependency (destination of the relocated util-layer \
         helpers)."
    );
}

#[test]
fn no_source_file_references_amplihack_cli() {
    // Includes the doc-comment at context_loaders.rs:70 ("Mirrors
    // `amplihack_cli::VERSION`") — the acceptance criteria require that comment
    // be updated to a crate-agnostic reference, so ANY textual occurrence of
    // the `amplihack_cli` token is a failure.
    let src = crate_root().join("src");
    let mut files = Vec::new();
    collect_rs_files(&src, &mut files);
    assert!(
        !files.is_empty(),
        "sanity check: expected to find .rs files under {}",
        src.display()
    );

    let mut offenders: Vec<String> = Vec::new();
    for file in &files {
        let contents = fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("failed to read {}: {e}", file.display()));
        for (idx, line) in contents.lines().enumerate() {
            if line.contains("amplihack_cli") {
                let rel = file.strip_prefix(&src).unwrap_or(file);
                offenders.push(format!(
                    "src/{}:{}: {}",
                    rel.display(),
                    idx + 1,
                    line.trim()
                ));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "issue #875: no source file under crates/amplihack-hooks/src may \
         reference the `amplihack_cli` crate (including comments), but found \
         {} occurrence(s):\n{}",
        offenders.len(),
        offenders.join("\n")
    );
}
