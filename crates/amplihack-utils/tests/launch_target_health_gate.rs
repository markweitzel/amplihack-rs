//! Integration tests for the launch-target health gate (issue #1266, Task A).
//!
//! These drive the real I/O shell against a temp-dir fixture, through the
//! `resolve_from_candidates` seam, so they never mutate process environment
//! (`std::env::set_var` is `unsafe` under edition 2024).
//!
//! The behaviour under test is the one that was missing on 2026-08-21, when
//! amplihack logged
//!
//! ```text
//! INFO launching claude binary=/home/azureuser/.npm-global/bin/claude version="unknown"
//! ```
//!
//! and then executed a 500-byte shell stub, producing
//! `Exec format error (os error 8)`. It had the signal and proceeded anyway.
//! Health is a filter, never an annotation.

#![cfg(unix)]

use amplihack_utils::launch_target::{Rejection, TargetSource, resolve_from_candidates};
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

/// The placeholder `@anthropic-ai/claude-code` leaves at `bin/claude.exe` when
/// its postinstall is suppressed: 500 bytes, ASCII, no shebang. Verified byte
/// for byte on the dev VM.
fn write_stub(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let mut body = b"echo \"Error: claude native binary not installed.\" >&2\nexit 1\n".to_vec();
    body.resize(500, b' ');
    fs::write(&path, body).unwrap();
    make_executable(&path);
    path
}

/// A binary that answers `--version` with a parseable semver, padded past the
/// stub-shape threshold so the fast path defers to the probe.
fn write_healthy(dir: &Path, name: &str, version: &str) -> PathBuf {
    let path = dir.join(name);
    let padding = "#".repeat(8192);
    fs::write(
        &path,
        format!("#!/bin/sh\necho '{version} (Claude Code)'\nexit 0\n{padding}\n"),
    )
    .unwrap();
    make_executable(&path);
    path
}

/// Exits non-zero on `--version`: present, executable, and useless.
fn write_broken_prober(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let padding = "#".repeat(8192);
    fs::write(&path, format!("#!/bin/sh\nexit 3\n{padding}\n")).unwrap();
    make_executable(&path);
    path
}

/// Answers `--version` with something that carries no semver.
fn write_unparseable(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let padding = "#".repeat(8192);
    fs::write(&path, format!("#!/bin/sh\necho unknown\n{padding}\n")).unwrap();
    make_executable(&path);
    path
}

/// Hangs forever on `--version`.
fn write_hanging(dir: &Path, name: &str) -> PathBuf {
    let path = dir.join(name);
    let padding = "#".repeat(8192);
    fs::write(&path, format!("#!/bin/sh\nsleep 600\n{padding}\n")).unwrap();
    make_executable(&path);
    path
}

fn make_executable(path: &Path) {
    let mut perms = fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    fs::set_permissions(path, perms).unwrap();
}

fn rejection_for<'a>(rejected: &'a [(PathBuf, Rejection)], path: &Path) -> Option<&'a Rejection> {
    rejected.iter().find(|(p, _)| p == path).map(|(_, r)| r)
}

// ---------------------------------------------------------------------------
// The headline defect: never launch a stub
// ---------------------------------------------------------------------------

#[test]
fn a_stub_is_rejected_and_the_healthy_binary_behind_it_is_chosen() {
    let dir = tempfile::tempdir().unwrap();
    let stub_dir = dir.path().join("npm-global-bin");
    let good_dir = dir.path().join("usr-bin");
    fs::create_dir_all(&stub_dir).unwrap();
    fs::create_dir_all(&good_dir).unwrap();

    let stub = write_stub(&stub_dir, "claude");
    let good = write_healthy(&good_dir, "claude", "2.1.238");

    let resolution = resolve_from_candidates(
        "claude",
        &[
            (stub.clone(), TargetSource::AmplihackPrefix),
            (good.clone(), TargetSource::Path),
        ],
    );

    let target = resolution
        .target
        .expect("the healthy binary behind the stub must be found");
    assert_eq!(target.path, good);
    assert_eq!(target.version, "2.1.238");
    assert_eq!(target.source, TargetSource::Path);
    assert_eq!(
        rejection_for(&resolution.rejected, &stub),
        Some(&Rejection::StubShape)
    );
}

#[test]
fn a_stub_alone_yields_no_target_at_all() {
    // Not "a target with version: unknown". No target.
    let dir = tempfile::tempdir().unwrap();
    let stub = write_stub(dir.path(), "claude");

    let resolution =
        resolve_from_candidates("claude", &[(stub.clone(), TargetSource::AmplihackPrefix)]);

    assert!(
        resolution.target.is_none(),
        "amplihack must not execute a binary it could not verify"
    );
    assert_eq!(
        rejection_for(&resolution.rejected, &stub),
        Some(&Rejection::StubShape)
    );
}

#[test]
fn the_first_healthy_candidate_wins_not_the_first_found() {
    let dir = tempfile::tempdir().unwrap();
    let first = write_healthy(dir.path(), "claude-first", "2.1.237");
    let second = write_healthy(dir.path(), "claude-second", "2.1.238");

    let resolution = resolve_from_candidates(
        "claude",
        &[
            (first.clone(), TargetSource::Path),
            (second, TargetSource::AmplihackPrefix),
        ],
    );

    assert_eq!(resolution.target.unwrap().path, first);
}

// ---------------------------------------------------------------------------
// Every rejection reason, end to end
// ---------------------------------------------------------------------------

#[test]
fn a_missing_path_is_rejected_as_missing() {
    let dir = tempfile::tempdir().unwrap();
    let ghost = dir.path().join("claude");
    let resolution = resolve_from_candidates("claude", &[(ghost.clone(), TargetSource::Path)]);
    assert_eq!(
        rejection_for(&resolution.rejected, &ghost),
        Some(&Rejection::Missing)
    );
}

#[test]
fn a_dangling_symlink_is_missing_not_a_target() {
    let dir = tempfile::tempdir().unwrap();
    let link = dir.path().join("claude");
    std::os::unix::fs::symlink(dir.path().join("gone"), &link).unwrap();
    let resolution = resolve_from_candidates("claude", &[(link.clone(), TargetSource::Path)]);
    assert_eq!(
        rejection_for(&resolution.rejected, &link),
        Some(&Rejection::Missing)
    );
}

#[test]
fn a_live_symlink_is_followed_not_rejected() {
    // Every npm-installed claude on every host is a symlink into
    // lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe. Using
    // symlink_metadata (or any is_file() derived from it) would reject them
    // all, including amplihack's own install.
    let dir = tempfile::tempdir().unwrap();
    let real = write_healthy(dir.path(), "claude.exe", "2.1.238");
    let link = dir.path().join("claude");
    std::os::unix::fs::symlink(&real, &link).unwrap();

    let resolution =
        resolve_from_candidates("claude", &[(link.clone(), TargetSource::AmplihackPrefix)]);
    let target = resolution
        .target
        .expect("a symlinked npm install is the normal case, not a rejection");
    assert_eq!(target.path, link);
    assert_eq!(target.version, "2.1.238");
}

#[test]
fn a_directory_is_rejected_as_not_a_file() {
    let dir = tempfile::tempdir().unwrap();
    let subdir = dir.path().join("claude");
    fs::create_dir(&subdir).unwrap();
    let resolution = resolve_from_candidates("claude", &[(subdir.clone(), TargetSource::Path)]);
    assert_eq!(
        rejection_for(&resolution.rejected, &subdir),
        Some(&Rejection::NotAFile)
    );
}

#[test]
fn a_non_executable_file_is_rejected_without_a_probe() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_healthy(dir.path(), "claude", "2.1.238");
    let mut perms = fs::metadata(&path).unwrap().permissions();
    perms.set_mode(0o644);
    fs::set_permissions(&path, perms).unwrap();

    let resolution = resolve_from_candidates("claude", &[(path.clone(), TargetSource::Path)]);
    assert_eq!(
        rejection_for(&resolution.rejected, &path),
        Some(&Rejection::NotExecutable)
    );
}

#[test]
fn a_non_zero_version_probe_is_rejected() {
    let dir = tempfile::tempdir().unwrap();
    let path = write_broken_prober(dir.path(), "claude");
    let resolution = resolve_from_candidates("claude", &[(path.clone(), TargetSource::Path)]);
    assert!(resolution.target.is_none());
    assert_eq!(
        rejection_for(&resolution.rejected, &path),
        Some(&Rejection::ProbeFailed)
    );
}

#[test]
fn version_unknown_is_a_rejection_not_an_annotation() {
    // This is the exact signal amplihack had on 2026-08-21 and ignored.
    let dir = tempfile::tempdir().unwrap();
    let path = write_unparseable(dir.path(), "claude");
    let resolution = resolve_from_candidates("claude", &[(path.clone(), TargetSource::Path)]);
    assert!(
        resolution.target.is_none(),
        "a binary that cannot report its version is never launched"
    );
    assert_eq!(
        rejection_for(&resolution.rejected, &path),
        Some(&Rejection::UnparseableVersion)
    );
}

// ---------------------------------------------------------------------------
// SEC-4: bounded probing
// ---------------------------------------------------------------------------

#[test]
fn a_hanging_candidate_times_out_and_the_next_one_is_still_reached() {
    let dir = tempfile::tempdir().unwrap();
    let hang = write_hanging(dir.path(), "claude-hang");
    let good = write_healthy(dir.path(), "claude-good", "2.1.238");

    let started = std::time::Instant::now();
    let resolution = resolve_from_candidates(
        "claude",
        &[
            (hang.clone(), TargetSource::Path),
            (good.clone(), TargetSource::FallbackDir),
        ],
    );
    let elapsed = started.elapsed();

    assert_eq!(resolution.target.expect("must fall through").path, good);
    assert_eq!(
        rejection_for(&resolution.rejected, &hang),
        Some(&Rejection::ProbeTimedOut)
    );
    assert!(
        elapsed < std::time::Duration::from_secs(8),
        "one hung candidate must not stall the launch; took {elapsed:?}"
    );
}

#[test]
fn the_total_probe_budget_bounds_a_path_full_of_hanging_binaries() {
    // SEC-4: eight candidates at the per-candidate timeout would be 24s of
    // foreground hang. The total budget is what makes that impossible.
    let dir = tempfile::tempdir().unwrap();
    let candidates: Vec<_> = (0..12)
        .map(|i| {
            (
                write_hanging(dir.path(), &format!("claude-{i}")),
                TargetSource::Path,
            )
        })
        .collect();

    let started = std::time::Instant::now();
    let resolution = resolve_from_candidates("claude", &candidates);
    let elapsed = started.elapsed();

    assert!(resolution.target.is_none());
    assert!(
        elapsed < std::time::Duration::from_secs(15),
        "total probe budget must bound the whole pass; took {elapsed:?}"
    );
}

#[test]
fn probing_stops_at_the_first_healthy_candidate() {
    // The common case must be one subprocess, not a full sweep: nothing after
    // the winner may appear in the rejection list.
    let dir = tempfile::tempdir().unwrap();
    let good = write_healthy(dir.path(), "claude-good", "2.1.238");
    let hang = write_hanging(dir.path(), "claude-hang");

    let resolution = resolve_from_candidates(
        "claude",
        &[
            (good.clone(), TargetSource::Path),
            (hang.clone(), TargetSource::AmplihackPrefix),
        ],
    );

    assert_eq!(resolution.target.unwrap().path, good);
    assert!(
        rejection_for(&resolution.rejected, &hang).is_none(),
        "candidates after the winner must never be probed"
    );
}

// ---------------------------------------------------------------------------
// Explicit override
// ---------------------------------------------------------------------------

#[test]
fn a_broken_user_supplied_override_is_an_error_not_a_silent_demotion() {
    // If you point amplihack at a specific binary and it is broken, amplihack
    // says so rather than quietly launching a different one.
    let dir = tempfile::tempdir().unwrap();
    let stub = write_stub(dir.path(), "claude-override");
    let good = write_healthy(dir.path(), "claude-good", "2.1.238");

    let resolution = resolve_from_candidates(
        "claude",
        &[
            (
                stub.clone(),
                TargetSource::ExplicitOverride {
                    user_supplied: true,
                },
            ),
            (good, TargetSource::Path),
        ],
    );

    assert!(
        resolution.target.is_none(),
        "a broken user override must not silently fall through to another binary"
    );
    assert_eq!(
        rejection_for(&resolution.rejected, &stub),
        Some(&Rejection::StubShape)
    );
}

#[test]
fn a_broken_amplihack_set_override_falls_through() {
    // `configure_preferred_rustyclawd_binary` sets AMPLIHACK_CLAUDE_BINARY_PATH
    // in-process. That is a preference, not an instruction, so a broken value
    // must not turn a working `amplihack rustyclawd` into a hard failure.
    let dir = tempfile::tempdir().unwrap();
    let stub = write_stub(dir.path(), "claude-preferred");
    let good = write_healthy(dir.path(), "claude-good", "2.1.238");

    let resolution = resolve_from_candidates(
        "claude",
        &[
            (
                stub.clone(),
                TargetSource::ExplicitOverride {
                    user_supplied: false,
                },
            ),
            (good.clone(), TargetSource::Path),
        ],
    );

    assert_eq!(
        resolution.target.expect("must fall through").path,
        good,
        "an amplihack-set preference that is broken warns and continues"
    );
}

// ---------------------------------------------------------------------------
// The error surface (Defect 3)
// ---------------------------------------------------------------------------

#[test]
fn the_rejection_report_explains_a_total_failure_without_naming_architecture() {
    let dir = tempfile::tempdir().unwrap();
    let stub = write_stub(dir.path(), "claude");
    let resolution =
        resolve_from_candidates("claude", &[(stub.clone(), TargetSource::AmplihackPrefix)]);

    assert!(resolution.target.is_none());
    let report = resolution.rejection_report();
    assert!(
        report.contains(&stub.display().to_string()),
        "the report must name what it tried:\n{report}"
    );
    let lower = report.to_lowercase();
    assert!(
        lower.contains("npm install"),
        "the report must state a remedy:\n{report}"
    );
    for forbidden in ["exec format error", "os error 8", "architecture"] {
        assert!(
            !lower.contains(forbidden),
            "the report must not contain {forbidden:?}:\n{report}"
        );
    }
}

#[test]
fn an_empty_candidate_list_is_not_a_panic() {
    let resolution = resolve_from_candidates("claude", &[]);
    assert!(resolution.target.is_none());
    assert!(resolution.rejected.is_empty());
    let _ = resolution.rejection_report();
}
