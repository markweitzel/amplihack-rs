//! crates/amplihack-utils/tests/no_global_path_mutation.rs
//!
//! Contract: no unit test in this crate may mutate the process-global `$PATH`.
//!
//! Why this is a hard rule rather than a style preference:
//!
//! libtest runs this crate's unit tests on parallel threads of ONE process.
//! Several of them spawn helpers by bare name — `artifact_guard` and
//! `worktree` shell out to `git` both from their fixtures and from the
//! production code under test. `$PATH` is process-global, so a test that
//! points it at a nonexistent directory makes every concurrent bare-name
//! spawn fail with `ENOENT` ("No such file or directory (os error 2)").
//!
//! That is exactly what happened: `find_falls_back_to_npm_global_when_not_on_path`
//! set `PATH=/nonexistent-just-for-this-test`, and 15 unrelated
//! `artifact_guard` tests failed in `cargo test --workspace` while passing
//! when run alone. It reads as flakiness and is not — it is a deterministic
//! race that fires whenever the scheduler overlaps the two.
//!
//! The `env_lock` in `test_support` does NOT make this safe. It serialises env
//! *mutators* against each other; the bare-name spawners are *readers* and
//! never take it.
//!
//! If a test genuinely must exercise `$PATH` resolution, do it without
//! mutating the process: pick a needle name that cannot exist on the real
//! `$PATH` (the fallback test's approach), or thread an explicit search path
//! through the function under test.

use std::path::{Path, PathBuf};

fn rust_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let entries = std::fs::read_dir(dir).unwrap_or_else(|e| panic!("read {}: {e}", dir.display()));
    for entry in entries {
        let path = entry.expect("dir entry").path();
        if path.is_dir() {
            rust_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn no_unit_test_in_this_crate_clobbers_the_process_path() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);
    assert!(
        !files.is_empty(),
        "found no sources under {}",
        src.display()
    );

    let mut offenders = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        for (i, line) in text.lines().enumerate() {
            let trimmed = line.trim_start();
            // The doc-comment references in this very contract are prose, not code.
            if trimmed.starts_with("//") {
                continue;
            }
            if line.contains(r#"set_var("PATH""#) || line.contains(r#"remove_var("PATH""#) {
                offenders.push(format!("{}:{}", file.display(), i + 1));
            }
        }
    }

    assert!(
        offenders.is_empty(),
        "these lines mutate the process-global $PATH, which breaks concurrent \
         bare-name subprocess spawns (git) in sibling tests:\n  {}\n\nSee this \
         file's module docs for the supported alternatives.",
        offenders.join("\n  ")
    );
}

/// F-S5 / issue #1274 ratchet — this crate splits `$PATH` in exactly ONE
/// place.
///
/// The previous version of this scan allowed any number of walks and only
/// required an `is_absolute` test near each one. That is how F-S5 happened
/// twice: `binary_finder::search_path_dirs` was a second independent funnel
/// with its own copy of the filter, `docker_detector::which_docker_in` was a
/// third, and every copy is a place the next person has to re-derive the rule.
/// Requiring the filter at every site protects the sites; requiring ONE site
/// protects the rule.
///
/// The behavioural cases are pinned in `launch_target`'s own test module
/// (`path_dirs`, `split_path_var_of`, `split_path_var`) and in
/// `launch_target_health_gate.rs`. What this catches is a second walk
/// appearing at all.
#[test]
fn this_crate_splits_the_path_in_exactly_one_place() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_sources(&src, &mut files);

    let mut sites = Vec::new();
    for file in &files {
        let text = std::fs::read_to_string(file)
            .unwrap_or_else(|e| panic!("read {}: {e}", file.display()));
        // Whole-line comments only, matching the scan above: the prose in this
        // crate's doc comments discusses `split_paths` at length.
        for (i, line) in text.lines().enumerate() {
            if line.trim_start().starts_with("//") {
                continue;
            }
            if line.contains("split_paths(") {
                sites.push(format!("{}:{}: {}", file.display(), i + 1, line.trim()));
            }
        }
    }

    assert_eq!(
        sites.len(),
        1,
        "`$PATH` must be split in exactly one place in this crate — \
         `launch_target::split_path_var`. Every other walk goes through it, \
         so the empty-element rule is stated once instead of re-derived. \
         Found:\n  {}",
        sites.join("\n  ")
    );
    assert!(
        sites[0].contains("launch_target.rs"),
        "the one `$PATH` walk must be the `launch_target` seam; found {}",
        sites[0]
    );
}

/// F-S2 ratchet — the `$PATH` → candidate-directory seam keeps its filter.
///
/// The behavioural cases live in `launch_target`'s own test module, against
/// the pure `path_dirs` / `split_path_var` seams, precisely because this file
/// forbids the alternative: pinning it end-to-end would mean setting `PATH` on
/// the process, and the module docs above explain what that does to the
/// fifteen unrelated tests that spawn `git` by bare name.
///
/// A pure seam can be tested and can also be quietly bypassed — someone
/// reintroducing a direct `split_paths` walk in `candidate_paths` would pass
/// every `path_dirs` test while restoring the bug. This scan is the guard
/// against that: the seam must exist, must filter on absoluteness, and must be
/// the only place `candidate_paths` learns about `$PATH`.
#[test]
fn the_path_to_candidate_directory_seam_still_filters_relative_entries() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("launch_target.rs");
    let text =
        std::fs::read_to_string(&src).unwrap_or_else(|e| panic!("read {}: {e}", src.display()));

    let seam = fn_body(&text, "pub fn split_path_var(")
        .expect("launch_target must route $PATH through a pure `split_path_var` seam");
    assert!(
        seam.contains("is_absolute"),
        "the seam must be able to drop relative and empty $PATH entries: an \
         empty element is POSIX for the current directory, and the resulting \
         bare candidate is resolved by execvp from wherever amplihack happens \
         to be.\nGot:\n{seam}"
    );

    let dirs = fn_body(&text, "pub fn path_dirs(")
        .expect("launch_target must expose `path_dirs` as the search rule");
    assert!(
        dirs.contains("RelativeEntries::Drop"),
        "`path_dirs` is the *search* rule and must drop relative entries. \
         Callers that need them keep them by naming \
         `RelativeEntries::Keep` at the call, so a reader can see which sites \
         were audited.\nGot:\n{dirs}"
    );

    let candidates =
        fn_body(&text, "fn candidate_paths(").expect("launch_target must define candidate_paths");
    assert!(
        !candidates.contains("split_paths"),
        "`candidate_paths` must obtain its directories from the seam, not by \
         walking $PATH itself — a second walk reintroduces the relative \
         candidate the seam exists to remove.\nGot:\n{candidates}"
    );
}

/// Issue #1276 ratchet — `launch_target` carries no process-global mutable
/// state.
///
/// The module made `path_dirs` pure specifically to avoid hidden process
/// state, and then grew an `AtomicBool` latch anyway. The latch was untestable
/// by construction (a one-way flag cannot be exercised twice in one process),
/// needed a `#[cfg(test)]` reset hook to be tested at all, and changed the
/// answer for every later test in the binary. Its replacement,
/// [`OverrideOrigin`], is a parameter.
///
/// The memo (`RESOLUTION_MEMO`) is deliberately not caught: it is a cache
/// validated against the candidate list, so it cannot answer differently from
/// a fresh computation. Mutable state that can *change the answer* is what is
/// banned.
#[test]
fn launch_target_holds_no_process_global_latch() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("src")
        .join("launch_target.rs");
    let text =
        std::fs::read_to_string(&src).unwrap_or_else(|e| panic!("read {}: {e}", src.display()));

    let mut offenders = Vec::new();
    for (i, line) in text.lines().enumerate() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("//") {
            continue;
        }
        if trimmed.starts_with("static ") && (line.contains("Atomic") || line.contains("Cell<")) {
            offenders.push(format!("{}: {}", i + 1, trimmed));
        }
    }

    assert!(
        offenders.is_empty(),
        "issue #1276: `launch_target` must not carry process-global mutable \
         state. Pass the value as a parameter — the compiler then names every \
         call site that has to honour it, and tests cannot leak it into each \
         other.\n  {}",
        offenders.join("\n  ")
    );
    assert!(
        !text.contains("reset_override_amplihack_supplied"),
        "the `#[cfg(test)]` reset hook existed only to make the latch \
         testable. It must be deleted with the latch, not left unused."
    );
    assert!(
        text.contains("pub enum OverrideOrigin"),
        "the override origin must still be an explicit, named parameter type"
    );
}

/// Extract a function body by brace matching from its signature prefix.
fn fn_body(text: &str, signature: &str) -> Option<String> {
    let start = text.find(signature)?;
    let open = text[start..].find('{')? + start;
    let mut depth = 0usize;
    for (offset, ch) in text[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(text[open..=open + offset].to_string());
                }
            }
            _ => {}
        }
    }
    None
}
