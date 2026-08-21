//! Unit tests for `launch_target` — the single health-gated launch resolver.
//!
//! TDD (red phase) for issue #1266. Everything here is a **pure** decision
//! function or a filesystem-only helper: no environment mutation, no network,
//! no subprocesses. That is deliberate — `decide_launch_action` and
//! `decide_repair_action` carry the whole of Defect 2's fix and the purge
//! authorization model respectively, and neither should need `unsafe
//! env::set_var` to exercise (see commit 33f728d8 / #1084, which removed
//! unsafe env mutation from this workspace's tests).
//!
//! Wire this module from `launch_target.rs` with:
//!
//! ```ignore
//! #[cfg(test)]
//! #[path = "launch_target_tests.rs"]
//! mod tests;
//! ```
//!
//! See `docs/LAUNCH_TARGET_RESOLUTION.md` for the contract these tests pin.

use super::*;
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

fn working(version: &str, semver: Option<&str>) -> Health {
    Health::Working {
        version: version.to_string(),
        semver: semver.map(str::to_string),
    }
}

fn candidate(path: &str, source: Source, health: Health, ownership: Ownership) -> Candidate {
    Candidate {
        path: PathBuf::from(path),
        source,
        health,
        ownership,
    }
}

fn resolution(selected: Option<Candidate>, rejected: Vec<Candidate>) -> Resolution {
    Resolution { selected, rejected }
}

/// A healthy, amplihack-owned selection at `version`.
fn owned_selection(version: &str, semver: &str) -> Resolution {
    resolution(
        Some(candidate(
            "/home/u/.npm-global/bin/claude",
            Source::FallbackDir,
            working(version, Some(semver)),
            Ownership::AmplihackOwned,
        )),
        vec![],
    )
}

/// A healthy selection amplihack does NOT own — the `/usr/bin/claude` case
/// from the live evidence on dev.
fn external_selection(version: &str, semver: &str) -> Resolution {
    resolution(
        Some(candidate(
            "/usr/bin/claude",
            Source::Path,
            working(version, Some(semver)),
            Ownership::External,
        )),
        vec![],
    )
}

const NPM_BACKED_INTERACTIVE: LaunchContext = LaunchContext {
    npm_backed: true,
    interactive: true,
};

// ---------------------------------------------------------------------------
// extract_semver — CORRECTNESS half of Defect 2
// ---------------------------------------------------------------------------
//
// `sanitize_version` mangles "2.1.238 (Claude Code)" into "2.1.238ClaudeCode",
// which never compares equal to npm's "2.1.238". That is a second, independent
// always-stale loop on top of the wrong-binary one. It doubles as the display
// allowlist that replaces sanitize_version's terminal-injection defense.

#[test]
fn extract_semver_pulls_version_out_of_claude_version_banner() {
    assert_eq!(
        extract_semver("2.1.238 (Claude Code)").as_deref(),
        Some("2.1.238"),
        "the exact string `claude --version` prints must yield a bare semver; \
         sanitize_version produced `2.1.238ClaudeCode` here, which is what made \
         the version comparison permanently unequal"
    );
}

#[test]
fn extract_semver_accepts_bare_and_v_prefixed_versions() {
    assert_eq!(extract_semver("2.1.238").as_deref(), Some("2.1.238"));
    assert_eq!(extract_semver("v2.1.238").as_deref(), Some("2.1.238"));
    assert_eq!(extract_semver("claude 2.1.238").as_deref(), Some("2.1.238"));
}

#[test]
fn extract_semver_keeps_prerelease_suffix() {
    assert_eq!(
        extract_semver("1.0.0-beta.3 (Claude Code)").as_deref(),
        Some("1.0.0-beta.3")
    );
}

#[test]
fn extract_semver_returns_none_for_non_versions() {
    // Fail closed: no semver means no upgrade decision at all.
    assert_eq!(extract_semver(""), None);
    assert_eq!(extract_semver("unknown"), None);
    assert_eq!(
        extract_semver("2.1"),
        None,
        "two components is not a semver"
    );
    assert_eq!(
        extract_semver("Error: claude native binary not installed."),
        None,
        "the stub's own output must never parse as a version"
    );
}

#[test]
fn extract_semver_strips_terminal_injection() {
    // SEC-A15: extract_semver is the allowlist, not strip_ansi (which is
    // CSI-only and misses OSC / ESC c / DCS / bare CR).
    let hostile = "2.1.238\u{1b}]0;pwned\u{7}\r\u{1b}c";
    let got = extract_semver(hostile).expect("semver prefix should still parse");
    assert_eq!(got, "2.1.238");
    assert!(
        !got.contains('\u{1b}') && !got.contains('\r') && !got.contains('\u{7}'),
        "extract_semver output must contain no control bytes, got {got:?}"
    );
}

// ---------------------------------------------------------------------------
// npm_prefix_dir_from — deny-by-default (SEC-A5)
// ---------------------------------------------------------------------------

#[test]
fn npm_prefix_dir_from_home_is_dot_npm_global() {
    assert_eq!(
        npm_prefix_dir_from(Some(Path::new("/home/u"))),
        Some(PathBuf::from("/home/u/.npm-global"))
    );
}

#[test]
fn npm_prefix_dir_from_denies_unset_empty_and_relative_home() {
    assert_eq!(npm_prefix_dir_from(None), None, "HOME unset => no prefix");
    assert_eq!(
        npm_prefix_dir_from(Some(Path::new(""))),
        None,
        "HOME empty => no prefix"
    );
    assert_eq!(
        npm_prefix_dir_from(Some(Path::new("relative/home"))),
        None,
        "HOME relative => no prefix; a relative prefix would make containment \
         checks depend on the process cwd"
    );
}

#[test]
fn npm_prefix_dir_from_denies_filesystem_root() {
    assert_eq!(
        npm_prefix_dir_from(Some(Path::new("/"))),
        None,
        "HOME=/ would make the prefix /.npm-global with one component; \
         deny rather than authorize deletion near the root"
    );
}

// ---------------------------------------------------------------------------
// is_amplihack_owned — the SOLE authorization predicate (SEC-A1/A2)
// ---------------------------------------------------------------------------

#[test]
fn ownership_is_component_wise_not_string_prefix() {
    // SEC-A1: `~/.npm-global-backup/bin/claude` is a *string* prefix match on
    // `~/.npm-global` and must be a component-wise NON-match. Getting this
    // wrong authorizes amplihack to delete files in a directory it never
    // created.
    let tmp = tempfile::tempdir().unwrap();
    let prefix = tmp.path().join(".npm-global");
    let backup = tmp.path().join(".npm-global-backup");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    std::fs::create_dir_all(backup.join("bin")).unwrap();
    std::fs::write(prefix.join("bin").join("claude"), b"x").unwrap();
    std::fs::write(backup.join("bin").join("claude"), b"x").unwrap();

    assert!(
        is_amplihack_owned_under(Some(&prefix), &prefix.join("bin").join("claude")),
        "a binary inside the prefix is owned"
    );
    assert!(
        !is_amplihack_owned_under(Some(&prefix), &backup.join("bin").join("claude")),
        "`.npm-global-backup` is a string-prefix match and must NOT be owned"
    );
}

#[test]
fn ownership_canonicalizes_the_parent_not_the_symlink_target() {
    // SEC-A2: ownership answers "does this link live in our prefix";
    // health answers "is its target a stub". Collapsing the two lets a
    // symlink whose target points back into the prefix authorize itself.
    let tmp = tempfile::tempdir().unwrap();
    let prefix = tmp.path().join(".npm-global");
    let outside = tmp.path().join("elsewhere");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    std::fs::create_dir_all(&outside).unwrap();

    let real = prefix.join("bin").join("real-claude");
    std::fs::write(&real, b"x").unwrap();

    // A link OUTSIDE the prefix pointing INTO it is not owned.
    let link_outside = outside.join("claude");
    #[cfg(unix)]
    std::os::unix::fs::symlink(&real, &link_outside).unwrap();
    #[cfg(unix)]
    assert!(
        !is_amplihack_owned_under(Some(&prefix), &link_outside),
        "a link living outside the prefix must not become owned just because \
         its target is inside"
    );
}

#[test]
fn ownership_denies_when_prefix_is_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("claude");
    std::fs::write(&bin, b"x").unwrap();
    assert!(
        !is_amplihack_owned_under(None, &bin),
        "no resolvable prefix => deny; never fall through to owned"
    );
}

#[test]
fn ownership_denies_a_missing_path() {
    let tmp = tempfile::tempdir().unwrap();
    let prefix = tmp.path().join(".npm-global");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    assert!(
        !is_amplihack_owned_under(Some(&prefix), &prefix.join("bin").join("nope")),
        "an unresolvable path is denied, not assumed owned"
    );
}

// ---------------------------------------------------------------------------
// decide_launch_action — THIS IS THE DEFECT 2 FIX
// ---------------------------------------------------------------------------

#[test]
fn launch_action_is_launch_when_current() {
    let r = owned_selection("2.1.238 (Claude Code)", "2.1.238");
    assert_eq!(
        decide_launch_action(&r, Some("2.1.238"), NPM_BACKED_INTERACTIVE),
        LaunchAction::Launch
    );
}

#[test]
fn launch_action_upgrades_only_what_amplihack_owns() {
    let r = owned_selection("2.1.237 (Claude Code)", "2.1.237");
    assert_eq!(
        decide_launch_action(&r, Some("2.1.238"), NPM_BACKED_INTERACTIVE),
        LaunchAction::Upgrade {
            from: "2.1.237".to_string(),
            to: "2.1.238".to_string()
        }
    );
}

#[test]
fn launch_action_is_notice_only_for_a_stale_binary_amplihack_does_not_own() {
    // The crux of Defect 2. On dev, /usr/bin/claude (2.1.237) drove the
    // "upgrade needed" decision, amplihack installed into ~/.npm-global
    // (a different precedence), and the cycle repeated on every launch
    // forever. amplihack must NOT install over a binary it does not own —
    // doing so is what creates the shadow copy in the first place.
    let r = external_selection("2.1.237 (Claude Code)", "2.1.237");
    assert_eq!(
        decide_launch_action(&r, Some("2.1.238"), NPM_BACKED_INTERACTIVE),
        LaunchAction::NoticeOnly {
            from: "2.1.237".to_string(),
            to: "2.1.238".to_string()
        },
        "a stale External binary gets a notice and is launched as-is; \
         installing here recreates the three-way disagreement"
    );
}

#[test]
fn launch_action_never_installs_over_an_env_override() {
    // An env override is user-directed and never amplihack-owned.
    let r = resolution(
        Some(candidate(
            "/opt/claude/bin/claude",
            Source::EnvOverride,
            working("2.1.237 (Claude Code)", Some("2.1.237")),
            Ownership::External,
        )),
        vec![],
    );
    assert!(
        matches!(
            decide_launch_action(&r, Some("2.1.238"), NPM_BACKED_INTERACTIVE),
            LaunchAction::NoticeOnly { .. }
        ),
        "AMPLIHACK_CLAUDE_BINARY_PATH must never be installed over"
    );
}

#[test]
fn launch_action_installs_fresh_when_nothing_healthy_was_found() {
    let r = resolution(
        None,
        vec![candidate(
            "/home/u/.npm-global/bin/claude",
            Source::FallbackDir,
            Health::Broken(BrokenReason::Stub),
            Ownership::AmplihackOwned,
        )],
    );
    assert_eq!(
        decide_launch_action(&r, Some("2.1.238"), NPM_BACKED_INTERACTIVE),
        LaunchAction::InstallFresh
    );
}

#[test]
fn launch_action_fails_when_nothing_healthy_and_install_impossible() {
    let r = resolution(
        None,
        vec![candidate(
            "/usr/bin/claude",
            Source::Path,
            Health::Broken(BrokenReason::ProbeFailed),
            Ownership::External,
        )],
    );
    assert_eq!(
        decide_launch_action(
            &r,
            Some("2.1.238"),
            LaunchContext {
                npm_backed: false,
                interactive: true
            }
        ),
        LaunchAction::Fail,
        "no healthy candidate and no way to install => Fail, never exec"
    );
}

#[test]
fn launch_action_fails_closed_when_the_installed_version_has_no_semver() {
    // Health::Working guarantees a version *string*, not a parseable semver.
    // No semver => no comparison => no upgrade. Never "upgrade because we
    // couldn't tell", which is exactly the reinstall-forever loop.
    let r = resolution(
        Some(candidate(
            "/home/u/.npm-global/bin/claude",
            Source::FallbackDir,
            working("Claude Code (dev build)", None),
            Ownership::AmplihackOwned,
        )),
        vec![],
    );
    assert_eq!(
        decide_launch_action(&r, Some("2.1.238"), NPM_BACKED_INTERACTIVE),
        LaunchAction::Launch
    );
}

#[test]
fn launch_action_is_launch_when_latest_is_unknown() {
    let r = owned_selection("2.1.237 (Claude Code)", "2.1.237");
    assert_eq!(
        decide_launch_action(&r, None, NPM_BACKED_INTERACTIVE),
        LaunchAction::Launch,
        "registry unreachable must not trigger an install"
    );
}

#[test]
fn launch_action_does_not_upgrade_a_non_npm_backed_tool() {
    let r = owned_selection("2.1.237 (Claude Code)", "2.1.237");
    assert_eq!(
        decide_launch_action(
            &r,
            Some("2.1.238"),
            LaunchContext {
                npm_backed: false,
                interactive: true
            }
        ),
        LaunchAction::Launch
    );
}

// ---------------------------------------------------------------------------
// decide_repair_action — pure, total, deny-by-default (SEC-A5/A6)
// ---------------------------------------------------------------------------

#[test]
fn repair_completes_the_install_once_for_an_owned_broken_binary() {
    assert_eq!(
        decide_repair_action(
            Ownership::AmplihackOwned,
            &Health::Broken(BrokenReason::Stub),
            Source::FallbackDir,
            false,
        ),
        RepairAction::CompleteInstall
    );
}

#[test]
fn repair_purges_only_after_a_failed_repair_attempt() {
    assert_eq!(
        decide_repair_action(
            Ownership::AmplihackOwned,
            &Health::Broken(BrokenReason::Stub),
            Source::FallbackDir,
            true,
        ),
        RepairAction::Purge,
        "purge is the second step, never the first"
    );
}

#[test]
fn repair_treats_a_timed_out_probe_like_a_failed_one() {
    assert_eq!(
        decide_repair_action(
            Ownership::AmplihackOwned,
            &Health::Broken(BrokenReason::ProbeTimedOut),
            Source::FallbackDir,
            true,
        ),
        RepairAction::Purge
    );
}

#[test]
fn repair_never_touches_a_binary_amplihack_does_not_own() {
    for attempted in [false, true] {
        for source in [Source::Path, Source::FallbackDir, Source::EnvOverride] {
            assert_eq!(
                decide_repair_action(
                    Ownership::External,
                    &Health::Broken(BrokenReason::Stub),
                    source,
                    attempted,
                ),
                RepairAction::None,
                "External + {source:?} + attempted={attempted} must be a no-op"
            );
        }
    }
}

#[test]
fn repair_never_purges_an_env_override_even_inside_the_prefix() {
    // SEC-A5: Source::EnvOverride denies always. A user who pointed
    // AMPLIHACK_CLAUDE_BINARY_PATH at a file gets it left alone, full stop.
    assert_eq!(
        decide_repair_action(
            Ownership::AmplihackOwned,
            &Health::Broken(BrokenReason::Stub),
            Source::EnvOverride,
            true,
        ),
        RepairAction::None
    );
}

#[test]
fn repair_is_a_no_op_for_a_working_binary() {
    for attempted in [false, true] {
        assert_eq!(
            decide_repair_action(
                Ownership::AmplihackOwned,
                &working("2.1.238 (Claude Code)", Some("2.1.238")),
                Source::FallbackDir,
                attempted,
            ),
            RepairAction::None
        );
    }
}

// ---------------------------------------------------------------------------
// render_rejections — SEC-A16 display sanitization + one remedy per reason
// ---------------------------------------------------------------------------

#[test]
fn render_rejections_names_every_candidate_and_its_reason() {
    let r = resolution(
        None,
        vec![
            candidate(
                "/usr/bin/claude",
                Source::Path,
                Health::Broken(BrokenReason::ProbeFailed),
                Ownership::External,
            ),
            candidate(
                "/home/u/.npm-global/bin/claude",
                Source::FallbackDir,
                Health::Broken(BrokenReason::Stub),
                Ownership::AmplihackOwned,
            ),
            candidate(
                "/home/u/.local/bin/claude",
                Source::FallbackDir,
                Health::Broken(BrokenReason::NotExecutable),
                Ownership::External,
            ),
            candidate(
                "/opt/claude",
                Source::EnvOverride,
                Health::Broken(BrokenReason::ProbeTimedOut),
                Ownership::External,
            ),
        ],
    );
    let out = render_rejections(&r);
    for path in [
        "/usr/bin/claude",
        "/home/u/.npm-global/bin/claude",
        "/home/u/.local/bin/claude",
        "/opt/claude",
    ] {
        assert!(out.contains(path), "rendering must name {path}:\n{out}");
    }
    let lower = out.to_lowercase();
    for phrase in ["stub", "not executable", "probe failed", "timed out"] {
        assert!(
            lower.contains(phrase),
            "each BrokenReason needs its own remedy phrase; missing {phrase:?}:\n{out}"
        );
    }
}

#[test]
fn render_rejections_sanitizes_hostile_candidate_paths() {
    // A filename in a writable PATH directory is attacker-influenceable and
    // is sanitized nowhere today. strip_ansi is CSI-only: it does not touch
    // OSC (ESC ]), ESC c, DCS, or a bare CR.
    let hostile = "/tmp/evil\u{1b}]0;pwned\u{7}\r\u{1b}c/claude";
    let r = resolution(
        None,
        vec![candidate(
            hostile,
            Source::Path,
            Health::Broken(BrokenReason::Stub),
            Ownership::External,
        )],
    );
    let out = render_rejections(&r);
    for bad in ['\u{1b}', '\r', '\u{7}'] {
        assert!(
            !out.contains(bad),
            "rendered output must contain no control byte {bad:?}; got {out:?}"
        );
    }
    assert!(
        out.contains("evil"),
        "sanitizing must not blank the path entirely:\n{out}"
    );
}

// ---------------------------------------------------------------------------
// Probe budget (SEC-A14) — asserted, not left implicit as "10s x N"
// ---------------------------------------------------------------------------

#[test]
fn probe_budget_is_bounded() {
    assert_eq!(
        MAX_PROBE_CANDIDATES, 8,
        "candidates are de-duplicated by canonical path and capped; without a \
         cap the worst case is 10s x N subprocesses on the launch path"
    );
    assert!(VALIDATION_PROBE_TIMEOUT <= std::time::Duration::from_secs(10));
    assert!(
        VALIDATION_PROBE_TIMEOUT >= std::time::Duration::from_secs(5),
        "500ms (the discovery timeout) is far too tight for a 339MB binary's \
         cold first run, and a false `unknown` now means a rejected install"
    );
}
