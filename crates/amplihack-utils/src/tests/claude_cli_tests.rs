use super::*;

// ---------------------------------------------------------------------------
// parse_semver
// ---------------------------------------------------------------------------

#[test]
fn parse_semver_simple() {
    assert_eq!(parse_semver("1.2.3"), Some("1.2.3".into()));
}

#[test]
fn parse_semver_with_prefix() {
    assert_eq!(parse_semver("claude v1.0.23"), Some("1.0.23".into()));
}

#[test]
fn parse_semver_multiline() {
    let text = "Claude Code CLI\nversion 2.5.10\nmore stuff";
    assert_eq!(parse_semver(text), Some("2.5.10".into()));
}

#[test]
fn parse_semver_no_match() {
    assert_eq!(parse_semver("no version here"), None);
}

#[test]
fn parse_semver_partial() {
    assert_eq!(parse_semver("1.2"), None);
}

// ---------------------------------------------------------------------------
// is_newer
// ---------------------------------------------------------------------------

#[test]
fn is_newer_major() {
    assert!(is_newer("1.0.0", "2.0.0"));
}

#[test]
fn is_newer_minor() {
    assert!(is_newer("1.2.0", "1.3.0"));
}

#[test]
fn is_newer_patch() {
    assert!(is_newer("1.2.3", "1.2.4"));
}

#[test]
fn is_newer_same() {
    assert!(!is_newer("1.2.3", "1.2.3"));
}

#[test]
fn is_newer_older() {
    assert!(!is_newer("2.0.0", "1.9.9"));
}

#[test]
fn is_newer_with_v_prefix() {
    assert!(is_newer("v1.0.0", "v2.0.0"));
}

#[test]
fn is_newer_invalid_returns_false() {
    assert!(!is_newer("abc", "1.0.0"));
    assert!(!is_newer("1.0.0", "xyz"));
}

// ---------------------------------------------------------------------------
// VersionStatus serde round-trip
// ---------------------------------------------------------------------------

#[test]
fn version_status_current_serde() {
    let v = VersionStatus::Current("1.2.3".into());
    let json = serde_json::to_string(&v).expect("serialize");
    let deser: VersionStatus = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(deser, v);
}

#[test]
fn version_status_update_available_serde() {
    let v = VersionStatus::UpdateAvailable {
        current: "1.0.0".into(),
        latest: "2.0.0".into(),
    };
    let json = serde_json::to_string(&v).expect("serialize");
    let deser: VersionStatus = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(deser, v);
}

#[test]
fn version_status_unknown_serde() {
    let v = VersionStatus::Unknown;
    let json = serde_json::to_string(&v).expect("serialize");
    let deser: VersionStatus = serde_json::from_str(&json).expect("deserialize");
    assert_eq!(deser, VersionStatus::Unknown);
}

// ---------------------------------------------------------------------------
// get_claude_cli_path
// ---------------------------------------------------------------------------

#[test]
fn get_claude_cli_path_does_not_panic() {
    // The result depends on whether claude is installed; just exercise the
    // function and ensure it doesn't blow up.
    let _ = get_claude_cli_path();
}

// ---------------------------------------------------------------------------
// ClaudeCliError display
//
// Issue #1266 removed `NpmNotFound`, `InstallFailed`, and `ValidationFailed`
// along with the second npm installer that was the only thing that could
// construct them. `npm_global_dir` / `npm_global_bin` /
// `validate_binary` went with it: path resolution and the health probe now live
// in `launch_target`, and their tests live there too.
// ---------------------------------------------------------------------------

#[test]
fn error_display_process_error() {
    let e = ClaudeCliError::Process(crate::process::ProcessError::EmptyCommand);
    let msg = e.to_string();
    assert!(msg.contains("process error"), "{msg}");
}
