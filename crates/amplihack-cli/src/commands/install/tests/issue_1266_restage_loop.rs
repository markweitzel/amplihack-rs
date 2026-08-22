//! Issue #1266 — a gap a restage cannot close must not trigger a restage.
//!
//! `essential_files(Bundle)` gained `context/SYSTEM_PROMPT_APPEND.md` so that
//! `missing_framework_paths` would restage an existing install and deliver
//! issue #1265's feature. The same list is the trigger for
//! `ensure_framework_installed`, which runs on **every** launch — so when the
//! source that restage copies from predates the file, the restage cannot close
//! the gap, the gap is still there next launch, and amplihack copies the whole
//! bundle and rewrites settings.json forever. That is the "expensive work
//! repeated on every launch" defect issue #1266 exists to delete, re-created on
//! a different axis.
//!
//! These tests pin the source-aware rule and, just as importantly, pin it
//! against the **rendered** entry format that `missing_framework_paths`
//! actually produces. Classifying against a shape production never emits is
//! how the F-S5 tolerance bug survived its first fix.

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

/// Non-vacuity guard: the classification is fed the real output of
/// `missing_framework_paths`, not a hand-written string. If the rendered entry
/// format drifts, this fails while the hand-written cases above stay green.
#[test]
fn the_rule_is_applied_to_the_entry_format_missing_framework_paths_emits() {
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
    // Everything staged except issue #1265's fragment.

    let missing = missing_framework_paths(&claude_dir).unwrap();
    assert_eq!(
        missing.len(),
        1,
        "fixture should leave exactly one gap, got {missing:?}"
    );
    assert!(missing[0].starts_with(FRAGMENT), "got {:?}", missing[0]);

    let source_root = tmp.path().join("src");
    let stale = stale_source(&source_root);
    assert!(
        !framework_restage_needed(true, &missing, Some(&stale)),
        "a stale source must not restage on every launch: {missing:?}"
    );
    let current_root = tmp.path().join("src-current");
    let current = current_source(&current_root);
    assert!(
        framework_restage_needed(true, &missing, Some(&current)),
        "a current source must still restage to deliver the fragment"
    );
}
