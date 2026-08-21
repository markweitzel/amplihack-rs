//! Child-process PATH tests for issue #1266 (SEC-A18 / SEC-A19).
//!
//! TDD (red phase).
//!
//! Wire from `crates/amplihack-cli/src/commands/launch/mod.rs`:
//!
//! ```ignore
//! #[cfg(test)]
//! mod tests_env_launch_target;
//! ```
//!
//! `augment_claude_launch_env` used to prepend `~/.npm-global/bin` to the
//! child's PATH **unconditionally**. On the repo owner's WSL machine that
//! directory is the FIRST PATH entry, so the stub amplihack left there
//! shadows a working native install — breaking bare `claude` system-wide,
//! not just `amplihack claude`.
//!
//! New signature:
//!
//! ```ignore
//! pub(super) fn augment_claude_launch_env(
//!     env_builder: EnvBuilder,
//!     tool: &str,
//!     selected_bin_dir: Option<&Path>,
//!     ownership: Ownership,
//! ) -> EnvBuilder
//! ```
//!
//! `ownership` is a parameter rather than something the call site pre-applies
//! so the authorization decision stays inside one audited function, matching
//! the ownership-is-authorization model in
//! `docs/LAUNCH_TARGET_RESOLUTION.md`.

use super::*;
use crate::env_builder::EnvBuilder;
use crate::test_support::{home_env_lock, restore_home, set_home};
use amplihack_utils::launch_target::Ownership;
use std::path::Path;

fn path_entries(env: &std::collections::HashMap<String, String>) -> Vec<String> {
    env.get("PATH")
        .map(|p| p.split(':').map(str::to_string).collect())
        .unwrap_or_default()
}

fn with_home<T>(f: impl FnOnce(&Path) -> T) -> T {
    let _guard = home_env_lock()
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let home = tempfile::tempdir().unwrap();
    let original_home = set_home(home.path());
    let result = f(home.path());
    restore_home(original_home);
    result
}

#[test]
fn an_owned_selection_is_prepended_to_the_child_path() {
    let env = with_home(|home| {
        let bin = home.join(".npm-global").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        augment_claude_launch_env(
            EnvBuilder::new(),
            "claude",
            Some(bin.as_path()),
            Ownership::AmplihackOwned,
        )
        .build()
    });
    let entries = path_entries(&env);
    assert!(
        entries
            .first()
            .is_some_and(|e| e.ends_with(".npm-global/bin")),
        "an amplihack-owned selection is prepended, as before; PATH = {entries:?}"
    );
}

#[test]
fn no_selection_prepends_nothing_and_never_falls_back_to_the_npm_prefix() {
    // SEC-A18. Falling back to `~/.npm-global/bin` here is precisely how the
    // child ends up with a stub at PATH[0].
    let env = with_home(|_| {
        augment_claude_launch_env(EnvBuilder::new(), "claude", None, Ownership::External).build()
    });
    let entries = path_entries(&env);
    assert!(
        !entries.iter().any(|e| e.ends_with(".npm-global/bin")),
        "no selection => prepend NOTHING; PATH = {entries:?}"
    );
}

#[test]
fn an_external_selection_is_not_promoted_onto_the_child_path() {
    // SEC-A19: prepending someone else's directory promotes EVERY executable
    // in it (git, node, rg) for the child. Pass the validated absolute path
    // instead and leave the child's PATH alone.
    let env = with_home(|home| {
        let bin = home.join("usr-local-bin");
        std::fs::create_dir_all(&bin).unwrap();
        augment_claude_launch_env(
            EnvBuilder::new(),
            "claude",
            Some(bin.as_path()),
            Ownership::External,
        )
        .build()
    });
    let entries = path_entries(&env);
    assert!(
        !entries.iter().any(|e| e.ends_with("usr-local-bin")),
        "an External selection's directory must not be promoted; PATH = {entries:?}"
    );
}

#[test]
fn non_claude_tools_are_untouched() {
    let env = with_home(|home| {
        let bin = home.join(".npm-global").join("bin");
        std::fs::create_dir_all(&bin).unwrap();
        augment_claude_launch_env(
            EnvBuilder::new(),
            "copilot",
            Some(bin.as_path()),
            Ownership::AmplihackOwned,
        )
        .build()
    });
    assert!(
        !env.contains_key("CLAUDE_PLUGIN_ROOT"),
        "the claude-specific augmentation must stay claude-specific"
    );
    assert!(
        !path_entries(&env)
            .iter()
            .any(|e| e.ends_with(".npm-global/bin"))
    );
}
