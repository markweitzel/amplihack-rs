//! Integration tests for issue #1265 Option 3 — the shipped system-prompt
//! fragment and its delivery contract.
//!
//! TDD (red phase). Scope is Option 3 ONLY: options 4 and 5 (a runtime check
//! that surfaces when routing was requested but not performed, and its
//! escalation) are deliberately out of scope and are asserted as such below,
//! so #1265's acceptance list does not read as half-done.

use std::path::{Path, PathBuf};

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

const FRAGMENT_REL: &str = "amplifier-bundle/context/SYSTEM_PROMPT_APPEND.md";

// ---------------------------------------------------------------------------
// The fragment ships, and ships through the EXISTING install mechanism
// ---------------------------------------------------------------------------

#[test]
fn the_fragment_ships_in_the_bundle() {
    let path = repo_root().join(FRAGMENT_REL);
    assert!(
        path.is_file(),
        "the fragment must ship at {FRAGMENT_REL}; it is installed by the same \
         mechanism as every other amplihack context file"
    );
}

#[test]
fn the_fragment_is_installed_by_the_existing_context_mapping() {
    // Requirement: "installed by whatever mechanism already installs
    // amplihack's other context files. Find that mechanism; do not invent a
    // parallel one." That mechanism is the ("context", "context") entry in
    // BUNDLE_DIR_MAPPING, which populates ~/.amplihack/.claude/context/.
    let types = read_source("crates/amplihack-cli/src/commands/install/types.rs");
    assert!(
        types.contains("(\"context\", \"context\")"),
        "the context/ mapping must remain the delivery path"
    );
    assert!(
        !types.contains("SYSTEM_PROMPT_APPEND"),
        "no bespoke entry: the fragment rides the existing context/ mapping. \
         A second installer for one file is exactly the parallel mechanism the \
         requirement forbids."
    );
}

// ---------------------------------------------------------------------------
// Fragment content rules
// ---------------------------------------------------------------------------

#[test]
fn the_fragment_carries_the_secrets_in_argv_notice() {
    // SEC-B6: the contents appear in `ps` and /proc/<pid>/cmdline. The notice
    // lives in the file because the next person to edit it will not have read
    // docs/SYSTEM_PROMPT_APPEND.md.
    let text = read_source(FRAGMENT_REL).to_lowercase();
    assert!(
        text.contains("secret"),
        "the file must warn that its contents must never contain secrets"
    );
    assert!(
        text.contains("argument") || text.contains("argv") || text.contains("cmdline"),
        "the notice must say WHY: the contents are passed as a command-line \
         argument"
    );
}

#[test]
fn the_fragment_is_short() {
    // It rides in argv on every single launch and every word is context the
    // session pays for. ~160 words by design.
    let text = read_source(FRAGMENT_REL);
    let words = text.split_whitespace().count();
    assert!(
        words <= 260,
        "the fragment must stay short; got {words} words. A long fragment \
         burns context every session."
    );
    assert!(
        text.len() <= 16 * 1024,
        "the shipped fragment must be well under the 16 KiB argv cap"
    );
    assert!(
        !text.trim().is_empty(),
        "an empty fragment is silently skipped at load time"
    );
}

#[test]
fn the_fragment_states_the_routing_contract_and_beats_a_contrary_instruction() {
    // It must win an argument with a contrary generic line in the base system
    // prompt, not merely suggest routing.
    let text = read_source(FRAGMENT_REL).to_lowercase();
    assert!(
        text.contains("amplihack"),
        "the fragment must name amplihack as the source of the contract"
    );
    assert!(
        text.contains("routing"),
        "it must state the routing contract plainly"
    );
    assert!(
        text.contains("does not apply")
            || text.contains("already asked")
            || text.contains("satisf"),
        "it must explicitly override the contrary instruction rather than \
         restating amplihack's preference: the base system prompt wins ties"
    );
}

#[test]
fn the_fragment_names_the_shape_of_conflicting_instructions_not_their_text() {
    // R9: quoting a specific sentence from the base system prompt rots the
    // moment that sentence is reworded. Describing the category survives drift.
    let text = read_source(FRAGMENT_REL);
    for quoted in [
        "Do not call the AgentTool unless the user requested it",
        "Do not use workflows or deep-research unless the user requested it",
    ] {
        assert!(
            !text.contains(quoted),
            "the fragment must not quote base-prompt text verbatim ({quoted:?}); \
             name the shape of the restriction instead"
        );
    }
    let lower = text.to_lowercase();
    assert!(
        lower.contains("sub-agent") || lower.contains("subagent") || lower.contains("delegat"),
        "it must name the CATEGORY of restricted behavior (delegation to \
         sub-agents, workflows, skills)"
    );
}

#[test]
fn the_fragment_does_not_broaden_permissions() {
    // It narrows one thing only. It must not read as a license for
    // unrequested work, destructive actions, or scope expansion.
    let lower = read_source(FRAGMENT_REL).to_lowercase();
    assert!(
        lower.contains("does not license")
            || lower.contains("stands unchanged")
            || lower.contains("narrows one thing"),
        "the fragment must explicitly bound its own scope"
    );
}

// ---------------------------------------------------------------------------
// Wiring
// ---------------------------------------------------------------------------

#[test]
fn the_opt_out_env_var_is_wired() {
    let src = read_source("crates/amplihack-cli/src/commands/launch/command.rs");
    assert!(
        src.contains("AMPLIHACK_NO_SYSTEM_PROMPT_APPEND"),
        "AMPLIHACK_NO_SYSTEM_PROMPT_APPEND=1 must be honored"
    );
}

#[test]
fn injection_is_gated_on_the_existing_capability_matrix() {
    let src = read_source("crates/amplihack-cli/src/commands/launch/command.rs");
    assert!(
        src.contains("supports_append_prompt"),
        "the gate must read flags_for(binary).supports_append_prompt from \
         crates/amplihack-launcher/src/flag_matrix.rs — not a new table"
    );
}

#[test]
fn the_loader_does_not_go_through_resolve_asset() {
    // SEC-B1/B2 (BLOCKING). `resolve_asset`'s search_bases() ranks the first
    // cwd ancestor containing amplifier-bundle/ above ~/.amplihack.
    let src = read_source("crates/amplihack-cli/src/commands/launch/command.rs");
    let start = src
        .find("fn load_system_prompt_fragment")
        .expect("load_system_prompt_fragment must exist");
    let region = &src[start..];
    let end = region.find("\n}\n").map(|i| i + 2).unwrap_or(region.len());
    let body = &region[..end];
    assert!(
        !body.contains("resolve_asset") && !body.contains("resolve_bundle_asset"),
        "the fragment loader must consult only $AMPLIHACK_HOME and \
         $HOME/.amplihack. Routing it through the general asset search means \
         `git clone <hostile> && cd && amplihack claude` injects a stranger's \
         text at system-prompt authority. Got:\n{body}"
    );
    assert!(
        !body.contains("current_dir") && !body.contains("ancestors"),
        "no cwd, no ancestor walk. Got:\n{body}"
    );
    assert!(
        !body.contains("CARGO_MANIFEST_DIR"),
        "CARGO_MANIFEST_DIR is a build-time path that may not be under the \
         running user's control. Got:\n{body}"
    );
}

#[test]
fn the_flag_matrix_docstring_says_prompt_not_path() {
    // `--append-system-prompt <prompt>` takes TEXT. flag_matrix is the
    // canonical capability table this feature gates on, so an implementer
    // reads its docstring as the flag's contract. It currently says <path>.
    let src = read_source("crates/amplihack-launcher/src/flag_matrix.rs");
    assert!(
        !src.contains("--append-system-prompt <path>"),
        "the docstring on FlagSet::supports_append_prompt must be corrected to \
         `--append-system-prompt <prompt>`"
    );
    assert!(src.contains("--append-system-prompt <prompt>"));
}

// ---------------------------------------------------------------------------
// Scope
// ---------------------------------------------------------------------------

#[test]
fn options_four_and_five_are_documented_as_out_of_scope() {
    let doc = read_source("docs/SYSTEM_PROMPT_APPEND.md");
    assert!(
        doc.contains("Options 4 and 5") || doc.contains("Option 4"),
        "the doc must say plainly that #1265's Option 4/5 acceptance items are \
         deliberately not implemented, so the issue's checklist does not read \
         as half-done"
    );
}

#[test]
fn no_routing_verification_check_was_smuggled_in() {
    // Option 4 is "a check surfaces when routing was requested but not
    // performed". Explicitly out of scope for this PR.
    let src = read_source("crates/amplihack-cli/src/commands/launch/command.rs");
    for forbidden in ["routing_was_performed", "verify_routing", "routing_check"] {
        assert!(
            !src.contains(forbidden),
            "{forbidden} belongs to Option 4, which is out of scope"
        );
    }
}
