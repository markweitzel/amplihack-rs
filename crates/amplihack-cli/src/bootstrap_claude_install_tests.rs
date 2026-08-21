//! Unit tests for the `@anthropic-ai/claude-code` install path (Defect 1).
//!
//! TDD (red phase) for issue #1266.
//!
//! Wire this module from `bootstrap.rs` with:
//!
//! ```ignore
//! #[cfg(test)]
//! #[path = "bootstrap_claude_install_tests.rs"]
//! mod claude_install_tests;
//! ```
//!
//! Context (`docs/LAUNCH_TARGET_RESOLUTION.md`, "Installing `claude`
//! properly"): `@anthropic-ai/claude-code` ships its real binary as
//! platform-specific `optionalDependencies` and materializes it in a
//! `postinstall` (`node install.cjs`). Amplihack passes BOTH
//! `--ignore-scripts` and `--omit=optional`, and **either flag alone yields a
//! ~500-byte placeholder**. The fix is an explicit three-step in amplihack's
//! own source, mirroring the `@github/copilot` two-step ten lines away —
//! `run_npm_install` is not touched, so the #585 contract tests pass
//! unmodified.

use super::*;

// ---------------------------------------------------------------------------
// claude_platform_package — mirrors install.cjs::getPlatformKey (SEC-A20)
// ---------------------------------------------------------------------------

#[test]
fn claude_platform_package_covers_all_eight_keys() {
    let cases: &[(&str, &str, Libc, &str)] = &[
        (
            "linux",
            "x86_64",
            Libc::Glibc,
            "@anthropic-ai/claude-code-linux-x64",
        ),
        (
            "linux",
            "aarch64",
            Libc::Glibc,
            "@anthropic-ai/claude-code-linux-arm64",
        ),
        (
            "linux",
            "x86_64",
            Libc::Musl,
            "@anthropic-ai/claude-code-linux-x64-musl",
        ),
        (
            "linux",
            "aarch64",
            Libc::Musl,
            "@anthropic-ai/claude-code-linux-arm64-musl",
        ),
        (
            "macos",
            "x86_64",
            Libc::Glibc,
            "@anthropic-ai/claude-code-darwin-x64",
        ),
        (
            "macos",
            "aarch64",
            Libc::Glibc,
            "@anthropic-ai/claude-code-darwin-arm64",
        ),
        (
            "windows",
            "x86_64",
            Libc::Glibc,
            "@anthropic-ai/claude-code-win32-x64",
        ),
        (
            "windows",
            "aarch64",
            Libc::Glibc,
            "@anthropic-ai/claude-code-win32-arm64",
        ),
    ];
    for (os, arch, libc, expected) in cases {
        assert_eq!(
            claude_platform_package(os, arch, *libc),
            Some(*expected),
            "({os}, {arch}, {libc:?}) must map to {expected}"
        );
    }
}

#[test]
fn claude_platform_package_ignores_libc_off_linux() {
    // install.cjs only appends `-musl` on linux. A musl reading on macOS must
    // not invent `@anthropic-ai/claude-code-darwin-x64-musl`, which does not
    // exist on the registry.
    assert_eq!(
        claude_platform_package("macos", "aarch64", Libc::Musl),
        Some("@anthropic-ai/claude-code-darwin-arm64")
    );
}

#[test]
fn claude_platform_package_returns_none_for_unknown_platforms() {
    // SEC-A20: unknown => None => skip the platform step (non-fatal, as the
    // copilot path already does). Never a package name built from runtime
    // strings.
    for (os, arch) in [
        ("freebsd", "x86_64"),
        ("linux", "riscv64"),
        ("", ""),
        ("linux", "x86_64\u{0}"),
        ("../../etc", "x86_64"),
    ] {
        assert_eq!(
            claude_platform_package(os, arch, Libc::Glibc),
            None,
            "({os:?}, {arch:?}) must not resolve to a package"
        );
    }
}

#[test]
fn claude_platform_packages_are_static_literals() {
    // The return type being `Option<&'static str>` is the security control:
    // it makes "concatenate a detected string into a package spec"
    // unrepresentable. This test documents the intent so nobody "simplifies"
    // it to `Option<String>`.
    fn assert_static(_: Option<&'static str>) {}
    assert_static(claude_platform_package("linux", "x86_64", Libc::Glibc));
}

// ---------------------------------------------------------------------------
// detect_libc (SEC-A21 / A7)
// ---------------------------------------------------------------------------

#[test]
fn detect_libc_returns_a_value_and_defaults_to_glibc_when_ambiguous() {
    // Decisive check is the /lib/ld-musl-* glob (zero subprocesses).
    // Ambiguity defaults to Glibc; a wrong guess costs one wasted download,
    // is caught by the health gate, and triggers exactly one bounded retry
    // with the other libc.
    let detected = detect_libc();
    assert!(matches!(detected, Libc::Glibc | Libc::Musl));
    assert_eq!(
        other_libc(Libc::Glibc),
        Libc::Musl,
        "the alternate-libc retry must flip to the other value exactly once"
    );
    assert_eq!(other_libc(Libc::Musl), Libc::Glibc);
}

#[cfg(target_os = "linux")]
#[test]
fn detect_libc_on_this_glibc_host_reports_glibc() {
    // CI and the dev VM are Ubuntu x86_64 (glibc). If this ever runs on a
    // musl image the assertion below is the signal to look, not a flake.
    let musl_loader_present = std::fs::read_dir("/lib")
        .map(|entries| {
            entries
                .flatten()
                .any(|e| e.file_name().to_string_lossy().starts_with("ld-musl-"))
        })
        .unwrap_or(false);
    let expected = if musl_loader_present {
        Libc::Musl
    } else {
        Libc::Glibc
    };
    assert_eq!(detect_libc(), expected);
}

// ---------------------------------------------------------------------------
// The claude branch keys on exact equality (SEC-A22 / R18)
// ---------------------------------------------------------------------------
//
// This negative test is the ENTIRE basis for accepting that amplihack executes
// `install.cjs` at all. If the branch predicate ever widens past one exact
// `&'static str`, a hostile package name reaches a `node <script>` execution.

#[test]
fn only_the_exact_claude_code_package_triggers_the_postinstall_branch() {
    assert!(needs_claude_two_step("@anthropic-ai/claude-code"));

    for hostile in [
        "evil-@anthropic-ai/claude-code",
        "@anthropic-ai/claude-code-evil",
        "@anthropic-ai/claude-code ",
        " @anthropic-ai/claude-code",
        "@anthropic-ai/Claude-Code",
        "@anthropic-ai/claude-code/../../evil",
        "@github/copilot",
        "@openai/codex",
        "",
    ] {
        assert!(
            !needs_claude_two_step(hostile),
            "{hostile:?} must NOT take the install.cjs branch — the predicate \
             is `== npm_package_for_install(\"claude\")`, never contains() or \
             starts_with()"
        );
    }
}

#[test]
fn the_claude_branch_predicate_agrees_with_the_package_table() {
    // Single source of truth: the branch keys on the value the install table
    // returns, so the two can never drift.
    assert!(needs_claude_two_step(
        npm_package_for_install("claude").expect("claude must be npm-backed")
    ));
    assert!(!needs_claude_two_step(
        npm_package_for_install("copilot").expect("copilot must be npm-backed")
    ));
}

// ---------------------------------------------------------------------------
// The copilot path is untouched (HARD CONSTRAINT from the repo owner)
// ---------------------------------------------------------------------------

#[test]
fn copilot_platform_package_table_is_unchanged() {
    assert_eq!(
        copilot_platform_package("linux", "x86_64"),
        Some("@github/copilot-linux-x64")
    );
    assert_eq!(
        copilot_platform_package("macos", "aarch64"),
        Some("@github/copilot-darwin-arm64")
    );
    assert_eq!(copilot_platform_package("freebsd", "x86_64"), None);
}

// ---------------------------------------------------------------------------
// install.cjs is located from static components only (SEC-A7)
// ---------------------------------------------------------------------------

#[test]
fn claude_postinstall_script_path_is_built_from_static_components() {
    let prefix = std::path::Path::new("/tmp/prefix");
    let script = claude_postinstall_script(prefix);
    assert_eq!(
        script,
        prefix
            .join("lib")
            .join("node_modules")
            .join("@anthropic-ai")
            .join("claude-code")
            .join("install.cjs")
    );
    assert!(
        script.starts_with(prefix),
        "the script path must stay inside the prefix we installed into"
    );
}

#[test]
fn claude_postinstall_is_skipped_when_the_script_is_a_symlink() {
    // SEC-A8: a symlink at install.cjs means package tampering. Skip it —
    // non-fatal, the health gate is the enforcement point.
    let tmp = tempfile::tempdir().unwrap();
    let pkg_dir = tmp
        .path()
        .join("lib")
        .join("node_modules")
        .join("@anthropic-ai")
        .join("claude-code");
    std::fs::create_dir_all(&pkg_dir).unwrap();
    let evil = tmp.path().join("evil.cjs");
    std::fs::write(&evil, b"// nope\n").unwrap();
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(&evil, pkg_dir.join("install.cjs")).unwrap();
        assert!(
            !claude_postinstall_script_is_trusted(tmp.path()),
            "a symlinked install.cjs must not be executed"
        );
    }

    // A regular file at the expected location is trusted.
    let tmp2 = tempfile::tempdir().unwrap();
    let pkg_dir2 = tmp2
        .path()
        .join("lib")
        .join("node_modules")
        .join("@anthropic-ai")
        .join("claude-code");
    std::fs::create_dir_all(&pkg_dir2).unwrap();
    std::fs::write(pkg_dir2.join("install.cjs"), b"// real\n").unwrap();
    assert!(claude_postinstall_script_is_trusted(tmp2.path()));
}

#[test]
fn claude_postinstall_is_skipped_when_the_script_is_absent() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(
        !claude_postinstall_script_is_trusted(tmp.path()),
        "a missing install.cjs is a skip, not a failure"
    );
}
