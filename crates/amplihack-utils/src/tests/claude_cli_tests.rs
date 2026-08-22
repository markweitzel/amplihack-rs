use super::*;

// ---------------------------------------------------------------------------
// get_claude_cli_path
//
// This is the module's whole remaining surface, so this is its whole remaining
// test. Issue #1266 removed everything else:
//
// * The second npm installer — deliberately not named here, because
//   `claude_install_contract.rs` greps every workspace source for its name to
//   prove it is gone — and with it
//   `npm_global_dir` / `npm_global_bin` / `validate_binary`, plus the
//   `NpmNotFound` / `InstallFailed` / `ValidationFailed` error variants only it
//   could construct. Path resolution and the health probe now live in
//   `launch_target`, and their tests live there too.
// * `check_claude_version` and its private `parse_semver` / `is_newer` /
//   `get_installed_version` / `get_latest_published_version` cast — a second
//   version probe and a second npm registry query, unmemoized and unsanitized,
//   duplicating `launch_target` and `tool_update_check`. `ClaudeCliError` and
//   `VersionStatus` became unconstructible and went with them.
//
// The tests that covered those are deleted rather than retargeted. The
// `parse_semver` and `is_newer` cases are subsumed by
// `launch_target::extract_version`'s own tests; the `VersionStatus` serde
// round-trips and the `ClaudeCliError` display case only ever exercised derive
// macros on types nobody constructed.
// ---------------------------------------------------------------------------

#[test]
fn get_claude_cli_path_does_not_panic() {
    // The result depends on whether claude is installed; just exercise the
    // delegation into `launch_target::resolve` and ensure it doesn't blow up.
    let _ = get_claude_cli_path();
}
