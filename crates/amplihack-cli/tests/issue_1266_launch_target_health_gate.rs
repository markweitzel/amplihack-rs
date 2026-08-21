//! Integration tests for issue #1266 — the three launch-path defects.
//!
//! TDD (red phase). These cover the parts that need a real filesystem:
//! the health gate's structural filter, purge containment, the ENOEXEC
//! diagnostic, and source-level guards that the four divergent resolvers
//! actually collapsed into one.
//!
//! The three defects, from live evidence on the azlin "dev" VM (2026-08-21):
//!
//!   1. `--ignore-scripts` + `--omit=optional` suppress the postinstall that
//!      materializes claude's 339MB native binary, leaving a ~500-byte ASCII
//!      stub at `bin/claude.exe`.
//!   2. Version-check, install target, and exec target were three different
//!      binaries. A single launch printed "2.1.237 -> 2.1.238" from
//!      `/usr/bin/claude`, installed a stub into `~/.npm-global/bin`, and
//!      launched `~/.local/bin/claude`. Guaranteed re-download every launch.
//!   3. The stub was exec'd with `version="unknown"` already in hand, and the
//!      failure surfaced as `Exec format error (os error 8)`.
//!
//! Contract: `docs/LAUNCH_TARGET_RESOLUTION.md`.

use amplihack_utils::launch_target::{self, BrokenReason, Health, PurgeOutcome, Source};
use std::path::{Path, PathBuf};

// ---------------------------------------------------------------------------
// Fixtures
// ---------------------------------------------------------------------------

/// Repo root (two levels up from `crates/amplihack-cli`).
fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .parent()
        .and_then(Path::parent)
        .expect("repo root")
        .to_path_buf()
}

fn read_source(rel: &str) -> String {
    let path = repo_root().join(rel);
    std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()))
}

/// Extract a function body by brace-matching, mirroring the technique the
/// #585 contract test already uses on this same file.
fn fn_body<'a>(src: &'a str, signature: &str) -> &'a str {
    let start = src
        .find(signature)
        .unwrap_or_else(|| panic!("{signature} must exist"));
    let rest = &src[start..];
    let open = rest.find('{').expect("function must have a body");
    let mut depth = 0usize;
    for (idx, ch) in rest[open..].char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return &rest[..open + idx + 1];
                }
            }
            _ => {}
        }
    }
    panic!("unbalanced braces after {signature}");
}

#[cfg(unix)]
fn chmod_x(path: &Path) {
    use std::os::unix::fs::PermissionsExt;
    let mut perms = std::fs::metadata(path).unwrap().permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(path, perms).unwrap();
}

/// A shell script larger than the 4096-byte structural threshold, so it
/// reaches the version probe.
#[cfg(unix)]
fn write_large_script(path: &Path, body: &str) {
    let padding = "#".repeat(5000);
    std::fs::write(path, format!("#!/bin/sh\n{body}\n{padding}\n")).unwrap();
    chmod_x(path);
}

// ===========================================================================
// DEFECT 3 (a): the health gate rejects a stub WITHOUT executing it
// ===========================================================================

#[cfg(unix)]
#[test]
fn structural_filter_rejects_a_small_stub_without_running_it() {
    // SEC-A12. This is a security control, not an optimization: the
    // zero-subprocess structural filter is what stops a hostile ~500-byte
    // script sitting in PATH[0] from ever being executed. The marker file is
    // the proof — if the ordering is inverted, the script runs and the marker
    // appears.
    let tmp = tempfile::tempdir().unwrap();
    let marker = tmp.path().join("PROBE_EXECUTED");
    let stub = tmp.path().join("claude");
    std::fs::write(
        &stub,
        format!(
            "#!/bin/sh\ntouch '{}'\necho 'Error: claude native binary not installed.'\nexit 1\n",
            marker.display()
        ),
    )
    .unwrap();
    chmod_x(&stub);
    assert!(
        std::fs::metadata(&stub).unwrap().len() < 4096,
        "fixture must be under the structural threshold"
    );

    let health = launch_target::probe_health(&stub);

    assert_eq!(health, Health::Broken(BrokenReason::Stub));
    assert!(
        !marker.exists(),
        "the structural filter must run BEFORE the version probe — the \
         candidate was executed"
    );
}

#[cfg(unix)]
#[test]
fn structural_filter_follows_the_symlink_before_measuring() {
    // The real stub is a symlink from `<prefix>/bin/claude` into
    // `lib/node_modules/@anthropic-ai/claude-code/bin/claude.exe`. Size and
    // magic checks must target the RESOLVED file, not the link.
    let tmp = tempfile::tempdir().unwrap();
    let pkg_bin = tmp
        .path()
        .join("lib/node_modules/@anthropic-ai/claude-code/bin");
    std::fs::create_dir_all(&pkg_bin).unwrap();
    let target = pkg_bin.join("claude.exe");
    std::fs::write(&target, "Error: claude native binary not installed.\n").unwrap();
    chmod_x(&target);

    let bin = tmp.path().join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let link = bin.join("claude");
    std::os::unix::fs::symlink(&target, &link).unwrap();

    assert_eq!(
        launch_target::probe_health(&link),
        Health::Broken(BrokenReason::Stub),
        "a symlink pointing at a stub is a stub"
    );
}

#[cfg(unix)]
#[test]
fn health_gate_rejects_a_non_executable_candidate() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("claude");
    // ELF magic + enough bytes to clear the structural filter, but no +x.
    let mut content = b"\x7fELF".to_vec();
    content.extend(std::iter::repeat_n(0u8, 8192));
    std::fs::write(&bin, content).unwrap();

    assert_eq!(
        launch_target::probe_health(&bin),
        Health::Broken(BrokenReason::NotExecutable)
    );
}

#[cfg(unix)]
#[test]
fn health_gate_rejects_a_candidate_whose_version_probe_fails() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("claude");
    write_large_script(&bin, "exit 127");

    assert_eq!(
        launch_target::probe_health(&bin),
        Health::Broken(BrokenReason::ProbeFailed)
    );
}

#[cfg(unix)]
#[test]
fn health_gate_accepts_a_binary_that_reports_a_version() {
    let tmp = tempfile::tempdir().unwrap();
    let bin = tmp.path().join("claude");
    write_large_script(&bin, "echo '2.1.238 (Claude Code)'");

    match launch_target::probe_health(&bin) {
        Health::Working { version, semver } => {
            assert!(version.contains("2.1.238"), "got version {version:?}");
            assert_eq!(
                semver.as_deref(),
                Some("2.1.238"),
                "Working.semver must be extract_semver(version), not \
                 sanitize_version — the latter yields `2.1.238ClaudeCode`, \
                 which never equals npm's `2.1.238`"
            );
        }
        other => panic!("expected Working, got {other:?}"),
    }
}

// ===========================================================================
// DEFECT 3 (b): an unhealthy candidate is never SELECTED
// ===========================================================================

#[cfg(unix)]
#[test]
fn resolve_never_selects_a_broken_env_override() {
    // `resolve` is infallible: a failed candidate is data in `rejected`,
    // never an Err and never a selection. The log line
    // `launching claude binary=... version="unknown"` is precisely the state
    // this makes unrepresentable.
    let _guard = env_guard();
    let tmp = tempfile::tempdir().unwrap();
    let stub = tmp.path().join("claude");
    std::fs::write(&stub, "Error: claude native binary not installed.\n").unwrap();
    chmod_x(&stub);

    let previous = std::env::var_os("AMPLIHACK_CLAUDE_BINARY_PATH");
    unsafe { std::env::set_var("AMPLIHACK_CLAUDE_BINARY_PATH", &stub) };
    let resolution = launch_target::resolve("claude");
    match previous {
        Some(v) => unsafe { std::env::set_var("AMPLIHACK_CLAUDE_BINARY_PATH", v) },
        None => unsafe { std::env::remove_var("AMPLIHACK_CLAUDE_BINARY_PATH") },
    }

    let canonical_stub = stub.canonicalize().unwrap();
    assert_ne!(
        resolution.selected.as_ref().map(|c| c.path.clone()),
        Some(canonical_stub.clone()),
        "a stub must never be selected, even when named explicitly"
    );
    let rejected = resolution
        .rejected
        .iter()
        .find(|c| c.path == canonical_stub)
        .expect("the stub must appear in `rejected` with a reason");
    assert_eq!(rejected.health, Health::Broken(BrokenReason::Stub));
    assert_eq!(rejected.source, Source::EnvOverride);

    // Invariant: anything selected is Working, with a real version string.
    if let Some(sel) = &resolution.selected {
        assert!(
            matches!(sel.health, Health::Working { .. }),
            "selected.is_some() => Health::Working"
        );
    }
}

fn env_guard() -> std::sync::MutexGuard<'static, ()> {
    static LOCK: std::sync::OnceLock<std::sync::Mutex<()>> = std::sync::OnceLock::new();
    LOCK.get_or_init(|| std::sync::Mutex::new(()))
        .lock()
        .unwrap_or_else(|p| p.into_inner())
}

// ===========================================================================
// Purge containment (SEC-A3/A4/A5, R3/R12) — the only destructive op
// ===========================================================================

#[cfg(unix)]
#[test]
fn purge_removes_the_symlink_and_never_its_target() {
    // The stub is a symlink. `symlink_metadata` + `remove_file` must delete
    // the LINK. A follow-then-delete would destroy whatever it points at.
    let tmp = tempfile::tempdir().unwrap();
    let prefix = tmp.path().join(".npm-global");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();
    let outside_target = tmp.path().join("precious.txt");
    std::fs::write(&outside_target, b"do not delete me").unwrap();

    let link = prefix.join("bin").join("claude");
    std::os::unix::fs::symlink(&outside_target, &link).unwrap();

    assert_eq!(
        launch_target::purge_binary_under(Some(&prefix), &link),
        PurgeOutcome::Removed
    );
    assert!(!link.exists() && link.symlink_metadata().is_err());
    assert!(
        outside_target.exists(),
        "purge must remove the link, never follow it"
    );
}

#[cfg(unix)]
#[test]
fn purge_denies_anything_outside_the_prefix() {
    let tmp = tempfile::tempdir().unwrap();
    let prefix = tmp.path().join(".npm-global");
    std::fs::create_dir_all(prefix.join("bin")).unwrap();

    // String-prefix sibling — the classic containment bug.
    let backup = tmp.path().join(".npm-global-backup").join("bin");
    std::fs::create_dir_all(&backup).unwrap();
    let victim = backup.join("claude");
    std::fs::write(&victim, b"someone else's file").unwrap();

    assert_eq!(
        launch_target::purge_binary_under(Some(&prefix), &victim),
        PurgeOutcome::Denied
    );
    assert!(
        victim.exists(),
        "nothing outside the prefix is ever removed"
    );

    // Absolute outsider.
    let elsewhere = tmp.path().join("usr-bin-claude");
    std::fs::write(&elsewhere, b"system claude").unwrap();
    assert_eq!(
        launch_target::purge_binary_under(Some(&prefix), &elsewhere),
        PurgeOutcome::Denied
    );
    assert!(elsewhere.exists());
}

#[cfg(unix)]
#[test]
fn purge_denies_when_the_prefix_is_unknown() {
    let tmp = tempfile::tempdir().unwrap();
    let f = tmp.path().join("claude");
    std::fs::write(&f, b"x").unwrap();
    assert_eq!(
        launch_target::purge_binary_under(None, &f),
        PurgeOutcome::Denied,
        "deny-by-default: an unresolvable prefix authorizes nothing"
    );
    assert!(f.exists());
}

// ===========================================================================
// DEFECT 3 (c): the spawn error names the real cause
// ===========================================================================

#[cfg(unix)]
#[test]
fn spawn_failure_on_a_stub_names_the_file_not_the_cpu_architecture() {
    // Verbatim from the live incident:
    //   error: failed to spawn child process: Exec format error (os error 8)
    // That names nothing real and sends the reader hunting a CPU-architecture
    // problem that does not exist. ENOEXEC must be special-cased BEFORE the
    // generic renderer.
    use amplihack_cli::launcher::ManagedChild;

    let tmp = tempfile::tempdir().unwrap();
    let stub = tmp.path().join("claude");
    std::fs::write(&stub, "Error: claude native binary not installed.\n").unwrap();
    chmod_x(&stub);

    let err = ManagedChild::spawn(std::process::Command::new(&stub))
        .expect_err("a non-executable-format file must not spawn");
    let rendered = format!("{err:#}");

    assert!(
        rendered.contains(&stub.display().to_string()),
        "the message must name the file it failed on; got:\n{rendered}"
    );
    assert!(
        rendered.contains("not a runnable program"),
        "ENOEXEC must be translated into plain language; got:\n{rendered}"
    );
    let lower = rendered.to_lowercase();
    assert!(
        lower.contains("npm install") || lower.contains("reinstall") || lower.contains("rm "),
        "the message must state a remedy the reader can run; got:\n{rendered}"
    );
    assert!(
        !lower.contains("exec format error"),
        "the raw errno text is what sent the user hunting a CPU-arch problem; \
         it must not be the message. Got:\n{rendered}"
    );
}

// ===========================================================================
// DEFECT 1: the install is a real install, and copilot is untouched
// ===========================================================================

#[test]
fn run_npm_install_is_unchanged_and_carries_no_per_package_exception() {
    // HARD CONSTRAINT: the copilot npm path keeps behaving exactly as it does
    // today. `run_npm_install` gains no allowlist, no branch, no package
    // awareness — the claude exception lives one level up, in
    // `install_npm_package`, as a named auditable branch.
    let src = read_source("crates/amplihack-cli/src/bootstrap.rs");
    let body = fn_body(&src, "fn run_npm_install(");

    for required in ["--omit=optional", "--ignore-scripts", "\"-g\"", "--prefix"] {
        assert!(
            body.contains(required),
            "run_npm_install must still pass {required}. Got:\n{body}"
        );
    }
    for forbidden in [
        "claude",
        "anthropic",
        "allowlist",
        "allow_scripts",
        "ALLOW_SCRIPTS",
    ] {
        assert!(
            !body.to_lowercase().contains(&forbidden.to_lowercase()),
            "run_npm_install must stay package-agnostic; found {forbidden:?}. \
             Narrowing --ignore-scripts here would still yield a stub (the \
             optional dep is absent), and narrowing --omit=optional too would \
             reintroduce #585's hang for a package with 8 cross-platform \
             optional deps. Got:\n{body}"
        );
    }
}

#[test]
fn issue_585_contract_tests_are_still_present_and_unmodified_in_spirit() {
    // Acceptance criterion, not a constraint: the three #585 contract tests
    // pass WITHOUT being edited. This guard fails if a future change quietly
    // deletes or reworks them to make room for a flag relaxation.
    let src = read_source("crates/amplihack-cli/tests/issue_585_copilot_npm_hang.rs");
    for name in [
        "fn run_npm_install_uses_omit_optional()",
        "fn run_npm_install_does_not_use_os_cpu_flags()",
        "fn run_npm_install_still_uses_ignore_scripts()",
    ] {
        assert!(src.contains(name), "#585 contract test {name} must remain");
    }
    assert!(
        src.contains("run_npm_install must keep --ignore-scripts for security"),
        "the #585 contract assertion text must not be weakened"
    );
}

#[test]
fn install_npm_package_materializes_the_claude_native_binary() {
    // With both flags on, npm alone leaves a ~500-byte placeholder. The
    // install is only an install once the platform package is fetched and
    // install.cjs has hardlinked its binary over that placeholder.
    let src = read_source("crates/amplihack-cli/src/bootstrap.rs");
    let body = fn_body(&src, "fn install_npm_package(");

    assert!(
        body.contains("needs_claude_two_step"),
        "install_npm_package must branch on the exact-equality predicate"
    );
    assert!(
        body.contains("claude_platform_package"),
        "step 2: the platform package must be installed separately, exactly \
         as the copilot path already does"
    );
    assert!(
        body.contains("run_claude_postinstall") || body.contains("install.cjs"),
        "step 3: install.cjs must be executed — it is what materializes the \
         339MB native binary. Got:\n{body}"
    );
    assert!(
        body.contains("copilot_platform_package"),
        "the copilot two-step must remain untouched"
    );
}

// ===========================================================================
// DEFECT 2: one resolver, not four
// ===========================================================================

#[test]
fn the_always_stale_npm_list_version_check_is_gone() {
    // `npm list -g` with NO --prefix resolves to /usr — the mechanical root
    // cause of "reads a different binary than it installs".
    let version_src = read_source("crates/amplihack-cli/src/tool_update_check/version.rs");
    assert!(
        !version_src.contains("fn get_installed_version"),
        "get_installed_version must be deleted, not merely bypassed; leaving \
         it live invites a caller to reintroduce the defect"
    );
    assert!(
        !version_src.contains("\"list\""),
        "no `npm list -g` version probe may remain in version.rs"
    );

    let bootstrap_src = read_source("crates/amplihack-cli/src/bootstrap.rs");
    assert!(
        !bootstrap_src.contains("get_installed_version"),
        "maybe_upgrade_tool must read the resolver's probed version"
    );
    let update_src = read_source("crates/amplihack-cli/src/tool_update_check/mod.rs");
    assert!(
        !update_src.contains("get_installed_version"),
        "maybe_print_npm_update_notice must read the resolver's probed version"
    );
}

#[test]
fn every_resolver_delegates_to_launch_target() {
    // Four independent `.npm-global` computations and four independent
    // notions of "the claude binary" are what let check, install, and exec
    // disagree three ways in a single launch.
    for (rel, what) in [
        ("crates/amplihack-cli/src/bootstrap.rs", "bootstrap"),
        ("crates/amplihack-utils/src/claude_cli.rs", "claude_cli"),
        (
            "crates/amplihack-cli/src/commands/launch/command.rs",
            "launch::command",
        ),
    ] {
        let src = read_source(rel);
        assert!(
            src.contains("launch_target"),
            "{what} must delegate to launch_target rather than keep its own \
             resolution"
        );
    }

    // `claude_cli` must lose its private, second `--ignore-scripts` install.
    let claude_cli = read_source("crates/amplihack-utils/src/claude_cli.rs");
    assert!(
        !claude_cli.contains("--ignore-scripts"),
        "claude_cli must not carry a parallel npm install path; that is the \
         fourth resolver"
    );

    // Only one definition of the npm prefix survives.
    let utils_prefix = read_source("crates/amplihack-utils/src/launch_target.rs");
    assert!(
        utils_prefix.contains(".npm-global"),
        "launch_target owns the single definition of the npm prefix"
    );
    for rel in [
        "crates/amplihack-cli/src/bootstrap.rs",
        "crates/amplihack-utils/src/claude_cli.rs",
        "crates/amplihack-cli/src/commands/launch/command.rs",
        "crates/amplihack-utils/src/binary_finder.rs",
    ] {
        assert!(
            !read_source(rel).contains(".npm-global"),
            "{rel} must call launch_target::npm_prefix_dir() instead of \
             recomputing `.npm-global`"
        );
    }
}

#[test]
fn the_child_path_is_not_seeded_from_the_stub_directory() {
    // SEC-A18/A19: augment_claude_launch_env used to prepend ~/.npm-global/bin
    // unconditionally — handing the child a PATH whose first entry is the
    // stub. On the repo owner's WSL box that directory is PATH[0], so the stub
    // shadows a working native install for `claude` system-wide.
    let src = read_source("crates/amplihack-cli/src/commands/launch/command.rs");
    let body = fn_body(&src, "fn augment_claude_launch_env(");
    assert!(
        body.contains("selected_bin_dir"),
        "augment_claude_launch_env must take the selected, validated binary's \
         directory. Got:\n{body}"
    );
    assert!(
        !body.contains(".npm-global"),
        "it must never fall back to the npm prefix. Got:\n{body}"
    );
}

#[test]
fn the_launch_site_refuses_to_exec_an_unversioned_binary() {
    // Defect 3's root: `version.unwrap_or("unknown")` discarded the one signal
    // that would have prevented the crash.
    let src = read_source("crates/amplihack-cli/src/commands/launch/mod.rs");
    assert!(
        !src.contains("unwrap_or(\"unknown\")"),
        "a version probe that fails, times out, or returns unknown is a FAILED \
         install; such a binary is never exec'd"
    );
    assert!(
        src.contains("render_rejections"),
        "when nothing healthy is found, the error must name every rejected \
         candidate and its reason"
    );
}

#[test]
fn ensure_tool_available_keeps_its_scanned_remedy_literals() {
    // R2: issue_585_copilot_npm_hang.rs:205-207 scans the SOURCE TEXT of
    // ensure_tool_available for one of three literals. Extracting the message
    // wholesale into render_rejections empties the body and fails that test
    // even though user-visible output is identical. Keep one literal in-body.
    let src = read_source("crates/amplihack-cli/src/bootstrap.rs");
    let body = fn_body(&src, "fn ensure_tool_available(");
    assert!(
        body.contains("PATH") || body.contains("npm install") || body.contains("Try running"),
        "ensure_tool_available's body must retain an actionable literal"
    );
    assert!(
        !body.contains("install-tool"),
        "`amplihack install-tool` is not a subcommand — the only `install_tool` \
         is a private fn in this file. Do not print a command that does not exist."
    );
}
