//! Issue #1266 — a gap a restage cannot close must not trigger a restage.
//!
//! `essential_files(Bundle)` briefly gained `context/SYSTEM_PROMPT_APPEND.md`
//! so `missing_framework_paths` would restage an existing install and deliver
//! issue #1265's feature. The same list is the trigger for
//! `ensure_framework_installed`, which runs on **every** launch — so when the
//! source that restage copies from predates the file, the restage cannot close
//! the gap, the gap is still there next launch, and amplihack copies the whole
//! bundle and rewrites settings.json forever. That is the "expensive work
//! repeated on every launch" defect issue #1266 exists to delete, re-created on
//! a different axis.
//!
//! Review found the listing had a second and worse consequence — the restage it
//! armed sources from a walk up from `current_dir()`, so a cloned fork could
//! write `$HOME` and have its bytes injected at system-prompt privilege — and
//! the fragment is now `include_str!`d into the binary instead. No file, no
//! listing, no trigger.
//!
//! These tests are kept and still wired. The source-aware rule is what makes
//! the *next* addition to `essential_files` safe by default, and this branch is
//! the proof that the mistake is easy to make. They pin the rule and, just as
//! importantly, pin it against the **rendered** entry format that
//! `missing_framework_paths` actually produces — classifying against a shape
//! production never emits is how the F-S5 tolerance bug survived its first fix.

use super::*;
use std::fs;
use std::path::{Path, PathBuf};

const FRAGMENT: &str = "context/SYSTEM_PROMPT_APPEND.md";

/// The exact shape `missing_framework_paths` emits.
fn rendered_gap(relative: &str, claude_dir: &Path) -> String {
    format!(
        "{relative} (expected at {})",
        claude_dir.join(relative).display()
    )
}

/// A source tree whose bundle predates the fragment.
fn stale_source(root: &Path) -> PathBuf {
    fs::create_dir_all(root.join("amplifier-bundle/context")).unwrap();
    root.to_path_buf()
}

/// A source tree whose bundle ships the fragment.
fn current_source(root: &Path) -> PathBuf {
    let bundle = root.join("amplifier-bundle");
    fs::create_dir_all(bundle.join("context")).unwrap();
    fs::write(bundle.join(FRAGMENT), "# contract\n").unwrap();
    root.to_path_buf()
}

#[test]
fn a_stale_source_bundle_does_not_trigger_a_restage_it_cannot_close() {
    let tmp = tempfile::tempdir().unwrap();
    let source = stale_source(tmp.path());
    let gap = rendered_gap(FRAGMENT, Path::new("/home/u/.amplihack/.claude"));

    assert!(!asset_gap_is_actionable(&gap, Some(&source)));
    assert!(!framework_restage_needed(
        true,
        std::slice::from_ref(&gap),
        Some(&source)
    ));
}

#[test]
fn a_current_source_bundle_still_triggers_the_restage_that_delivers_the_fragment() {
    let tmp = tempfile::tempdir().unwrap();
    let source = current_source(tmp.path());
    let gap = rendered_gap(FRAGMENT, Path::new("/home/u/.amplihack/.claude"));

    assert!(asset_gap_is_actionable(&gap, Some(&source)));
    assert!(framework_restage_needed(
        true,
        std::slice::from_ref(&gap),
        Some(&source)
    ));
}

#[test]
fn no_resolved_source_stays_actionable_because_run_install_can_fetch_one() {
    let gap = rendered_gap(FRAGMENT, Path::new("/home/u/.amplihack/.claude"));
    assert!(asset_gap_is_actionable(&gap, None));
    assert!(framework_restage_needed(
        true,
        std::slice::from_ref(&gap),
        None
    ));
}

#[test]
fn every_other_gap_still_triggers_a_restage_from_a_stale_source() {
    let tmp = tempfile::tempdir().unwrap();
    let source = stale_source(tmp.path());
    let claude_dir = Path::new("/home/u/.amplihack/.claude");

    // A genuinely required asset, a directory, and the transitional XPIA class
    // that really does self-heal: none of them may be exempted.
    for relative in [
        "tools/statusline.sh",
        "agents",
        "tools/xpia/hooks/pre_tool_use.sh",
    ] {
        let gap = rendered_gap(relative, claude_dir);
        assert!(
            asset_gap_is_actionable(&gap, Some(&source)),
            "{relative} must still trigger a restage"
        );
    }
}

#[test]
fn one_unclosable_gap_does_not_suppress_a_closable_one() {
    let tmp = tempfile::tempdir().unwrap();
    let source = stale_source(tmp.path());
    let claude_dir = Path::new("/home/u/.amplihack/.claude");
    let missing = vec![
        rendered_gap(FRAGMENT, claude_dir),
        rendered_gap("tools/statusline.sh", claude_dir),
    ];

    assert!(framework_restage_needed(true, &missing, Some(&source)));
}

#[test]
fn a_missing_staging_dir_always_bootstraps() {
    let tmp = tempfile::tempdir().unwrap();
    let source = stale_source(tmp.path());
    assert!(framework_restage_needed(false, &[], Some(&source)));
    assert!(framework_restage_needed(false, &[], None));
}

/// Non-vacuity guard, and the record of how this was actually fixed.
///
/// The classification above is fed hand-written strings. This one drives the
/// real `missing_framework_paths` so a drift in the rendered entry format is
/// caught — classifying against a shape production never emits is how the F-S5
/// tolerance bug survived its first fix.
///
/// It used to assert that a fully-staged Bundle install still reported the
/// fragment as a gap, because `essential_files(Bundle)` listed it. That listing
/// is gone: it armed `ensure_framework_installed` on every install in the
/// world, and `find_bundled_framework_root` sources the restage by walking up
/// from `current_dir()` — so a cloned fork could write `$HOME` and have its
/// bytes injected at system-prompt privilege. The fragment is `include_str!`d
/// into the binary now.
///
/// So the assertion inverts: a fully-staged Bundle install reports **no** gap,
/// and therefore no restage. The source-aware rule above is retained and still
/// wired (see the ratchet below) because it is what makes the *next* addition
/// to `essential_files` safe by default — this branch made exactly that mistake
/// once, and the mechanism that catches it should outlive the file that
/// prompted it.
#[test]
fn a_fully_staged_bundle_install_reports_no_gap_and_no_restage() {
    let tmp = tempfile::tempdir().unwrap();
    let claude_dir = tmp.path().join(".amplihack/.claude");
    fs::create_dir_all(&claude_dir).unwrap();
    write_layout_marker(&claude_dir, SourceLayout::Bundle).unwrap();
    for dir in essential_destinations(SourceLayout::Bundle) {
        fs::create_dir_all(claude_dir.join(dir)).unwrap();
    }
    fs::write(claude_dir.join("tools/statusline.sh"), "echo hi\n").unwrap();
    fs::write(tmp.path().join(".amplihack/CLAUDE.md"), "root\n").unwrap();
    let recipes = tmp.path().join(".amplihack/amplifier-bundle/recipes");
    fs::create_dir_all(&recipes).unwrap();
    for recipe in [
        "smart-orchestrator.yaml",
        "default-workflow.yaml",
        "investigation-workflow.yaml",
    ] {
        fs::write(recipes.join(recipe), "name: x\n").unwrap();
    }
    // Deliberately NOT staging the fragment: it is compiled in, so its absence
    // from disk must be a non-event.

    let missing = missing_framework_paths(&claude_dir).unwrap();
    assert!(
        missing.is_empty(),
        "the fragment must not be an essential file — listing it is what armed \
         a cwd-sourced restage of $HOME on every install. Got {missing:?}"
    );
    assert!(
        !framework_restage_needed(true, &missing, None),
        "no gap, no restage: {missing:?}"
    );

    // And the rendered-format guard the old fixture provided, kept alive
    // against the class the rule still covers.
    let gap = rendered_gap(FRAGMENT, &claude_dir);
    let stale = stale_source(&tmp.path().join("src"));
    assert!(
        !asset_gap_is_actionable(&gap, Some(&stale)),
        "the rule must still classify the real rendered entry format"
    );
}

/// Wiring ratchet.
///
/// Every test above exercises the pure predicates. Revert
/// `ensure_framework_installed`'s body to `!missing.is_empty()` and all of them
/// stay green — the loop comes back and nothing here notices. The only other
/// guard is that `framework_restage_needed` becomes dead code under
/// `-D warnings`, which is indirect and would evaporate the moment anything
/// else called it.
///
/// So scan the call site by shape, the same way this branch guards its other
/// wiring. Verified non-vacuous by deleting the call: this goes red.
#[test]
fn ensure_framework_installed_decides_through_the_source_aware_rule() {
    let src = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("src/commands/install/mod.rs");
    let text = fs::read_to_string(&src).unwrap_or_else(|e| panic!("read {}: {e}", src.display()));

    let body = text
        .split_once("pub(crate) fn ensure_framework_installed()")
        .expect("ensure_framework_installed moved — follow it, do not delete this scan")
        .1;
    // Bound the window to this function: the next `\npub` starts the following item.
    let body = body.split("\npub ").next().unwrap_or(body);

    assert!(
        body.contains("framework_restage_needed("),
        "ensure_framework_installed must decide through framework_restage_needed, \
         or the restage loop it exists to prevent is one edit away from returning"
    );
    assert!(
        !body.contains("!missing_framework_paths(&staging_dir)?.is_empty()"),
        "the raw emptiness check is the pre-fix trigger; it restages for gaps a \
         restage cannot close"
    );
}
