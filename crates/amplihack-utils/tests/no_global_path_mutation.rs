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

/// F-S2 ratchet — the `$PATH` → candidate-directory seam keeps its filter.
///
/// The behavioural cases live in `launch_target`'s own test module, against
/// the pure `path_dirs` seam, precisely because this file forbids the
/// alternative: pinning it end-to-end would mean setting `PATH` on the
/// process, and the module docs above explain what that does to the fifteen
/// unrelated tests that spawn `git` by bare name.
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

    let seam = fn_body(&text, "fn path_dirs(")
        .expect("launch_target must route $PATH through a pure `path_dirs` seam");
    assert!(
        seam.contains("is_absolute"),
        "`path_dirs` must drop relative and empty $PATH entries: an empty \
         element is POSIX for the current directory, and the resulting bare \
         candidate is resolved by execvp from wherever amplihack happens to \
         be.\nGot:\n{seam}"
    );

    let candidates =
        fn_body(&text, "fn candidate_paths(").expect("launch_target must define candidate_paths");
    assert!(
        !candidates.contains("split_paths"),
        "`candidate_paths` must obtain its directories from `path_dirs`, not \
         by walking $PATH itself — a second walk reintroduces the relative \
         candidate the seam exists to remove.\nGot:\n{candidates}"
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
