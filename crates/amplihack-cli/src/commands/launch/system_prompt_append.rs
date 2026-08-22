//! Issue #1265, Option 3 — deliver amplihack's routing contract through
//! `--append-system-prompt`.
//!
//! # Why this channel
//!
//! amplihack delivers its routing instructions through a `UserPromptSubmit`
//! hook and `CLAUDE.md`. Both are *content* the agent reads; the base system
//! prompt is *the frame it reads them in*. When the base prompt carries a
//! directly contrary line — "Do not call the AgentTool unless the user
//! requested it", "Do not use workflows or deep-research unless the user
//! requested it" — the system prompt wins, the router is silently ignored, and
//! amplihack's central promise stops holding with no error and no warning.
//!
//! That is a delivery-channel problem, not a wording problem. No amount of
//! rewording a structurally outranked channel fixes it. `--append-system-prompt`
//! puts amplihack's contract at the same privilege level as the instruction it
//! has to overcome.
//!
//! # Security
//!
//! * SEC-1 — the fragment is read from amplihack's own root **only**
//!   (`$HOME/.amplihack/.claude/context/`), never through
//!   `AmplihackPaths::resolve_framework_file`, which walks *up from the current
//!   directory first*. Under that resolver, `git clone <repo> && cd repo &&
//!   amplihack claude` would hand an attacker-authored file to the agent at
//!   system-prompt privilege — and it would inherit this fragment's own framing
//!   ("supersedes any earlier instruction", naming the guardrails it overrides)
//!   for free. The traversal and symlink guards in that resolver are sound; the
//!   precedence *order* is the problem, and it is correct-by-default for
//!   `USER_PREFERENCES.md` and wrong for this one file.
//! * SEC-6 — the read itself is bounded at [`MAX_FRAGMENT_BYTES`], not gated by
//!   a prior `metadata().len()` check. The 25-line cap is a test against the
//!   shipped file; at runtime the loader reads whatever is on disk. Without a
//!   bound, an oversized file (corrupted, tampered, or a stray `cat >>`) is
//!   passed whole into argv, and past `ARG_MAX` the spawn fails with `E2BIG` —
//!   a self-inflicted denial of the very launch graceful degradation exists to
//!   protect. A `stat`-then-read pair does not bound anything: the two syscalls
//!   resolve the path independently, and a FIFO at that path reports length 0
//!   and then delivers as much as its writer cares to send.
//! * A FIFO at the fragment path can still block the open. That is not an
//!   escalation and is deliberately not coded around: the path is under
//!   `$HOME/.amplihack`, and anyone who can create a FIFO there can instead
//!   write a regular file whose *contents* the agent will honour at
//!   system-prompt privilege, which is the strictly stronger capability.
//! * The fragment's bytes appear in the process table and are visible to every
//!   user on the host. It carries operating instructions, never secrets, and
//!   the shipped file says so in its own header.
//! * `AMPLIHACK_NO_SYSTEM_PROMPT_APPEND` is UX, not a security boundary —
//!   anyone who can set it can already exec.

use std::path::{Path, PathBuf};

/// Installed location of the fragment, relative to `$HOME`.
pub(crate) const FRAGMENT_RELATIVE_PATH: &str =
    ".amplihack/.claude/context/SYSTEM_PROMPT_APPEND.md";

/// Environment opt-out. Triggers on the exact value `"1"`, following the
/// `AMPLIHACK_COPILOT_NO_ALLOW_ALL` precedent.
pub(crate) const OPT_OUT_ENV: &str = "AMPLIHACK_NO_SYSTEM_PROMPT_APPEND";

/// SEC-6: refuse to inject a fragment larger than this.
pub(crate) const MAX_FRAGMENT_BYTES: u64 = 32 * 1024;

/// The flags a user can supply to take over the append channel.
///
/// Two forms, each also accepted with a trailing `=value` — see
/// [`user_supplied_append_flag`], which is where the `=` handling lives.
pub(crate) const USER_FLAG_FORMS: &[&str] =
    &["--append-system-prompt", "--append-system-prompt-file"];

/// Map a launcher binary name onto its `AgentBinary` variant.
///
/// `AgentBinary` has exactly four variants, so `rusty` and `rustyclawd` need
/// this mapping to be answerable by the flag matrix at all — `run_rustyclawd`
/// delegates to `run_launch("claude", "claude", ...)`, so they are
/// claude-compatible front ends.
///
/// An unrecognised name returns `None` and injects nothing. `None` is the safe
/// default: an unknown binary must never receive a flag it may not accept.
pub(crate) fn agent_binary_for_name(
    binary_name: &str,
) -> Option<amplihack_launcher::flag_matrix::AgentBinary> {
    use amplihack_launcher::flag_matrix::AgentBinary;
    match binary_name {
        "claude" | "rusty" | "rustyclawd" => Some(AgentBinary::Claude),
        "copilot" => Some(AgentBinary::Copilot),
        "codex" => Some(AgentBinary::Codex),
        "amplifier" => Some(AgentBinary::Amplifier),
        _ => None,
    }
}

/// Should amplihack inject `--append-system-prompt` into this launch?
///
/// Pure — no I/O, and **no `std::env` reads inside**. This is a deliberate
/// divergence from its neighbours [`super::command::should_inject_copilot_allow_all`]
/// and `should_inject_copilot_remote`, which read the environment internally.
/// The read is hoisted to the call site here so the function is directly
/// testable without mutating process environment, which is `unsafe` under
/// edition 2024. Do not move it back inside.
///
/// True iff **all** of:
///
/// 1. [`agent_binary_for_name`] maps the name to a binary whose
///    `flags_for(..).supports_append_prompt` is true. The flag matrix is the
///    single source of truth — see the Amplifier note in
///    `docs/SYSTEM_PROMPT_APPEND.md`.
/// 2. `opt_out` is not `Some("1")`.
/// 3. `fragment_present`.
/// 4. The user supplied none of the four `--append-system-prompt*` forms.
///
/// `fragment_present` is `&&`-ed in and never read again, so this is **monotone**
/// in that argument: `false` for `true` implies `false` for `false`. The call
/// site relies on exactly that to answer the question before paying for the
/// file read — see `build_command_for_dir`, and
/// `the_gate_is_monotone_in_fragment_present`, which pins it.
pub(crate) fn should_inject_system_prompt_append(
    binary_name: &str,
    extra_args: &[String],
    opt_out: Option<&str>,
    fragment_present: bool,
) -> bool {
    if !fragment_present || opt_out == Some("1") {
        return false;
    }
    let Some(binary) = agent_binary_for_name(binary_name) else {
        return false;
    };
    // The flag matrix is the source of truth, deliberately. `build_command_for_dir`'s
    // local `is_claude_compatible` also matches "amplifier" and governs
    // `--dangerously-skip-permissions` and `--model`; the matrix says
    // `supports_append_prompt == false` for Amplifier. The two disagree, the
    // matrix wins here, and `is_claude_compatible` is left alone — retargeting
    // those other two flags is a separate question. Do not "harmonize" them.
    if !amplihack_launcher::flag_matrix::flags_for(binary).supports_append_prompt {
        return false;
    }
    !user_supplied_append_flag(extra_args)
}

/// Did the caller already pass the flag themselves, in any accepted spelling?
///
/// Exact match on each form, or that form followed by `=`. Matching on a bare
/// prefix would make `--append-system-prompt-extra` and `--no-append-system-prompt`
/// false positives and silently disable the feature.
fn user_supplied_append_flag(extra_args: &[String]) -> bool {
    extra_args.iter().any(|arg| {
        USER_FLAG_FORMS.iter().any(|form| {
            arg == form
                || arg
                    .strip_prefix(form)
                    .is_some_and(|rest| rest.starts_with('='))
        })
    })
}

/// Absolute path of the installed fragment.
///
/// SEC-1: resolved from `$HOME` only. There is deliberately no `AMPLIHACK_ROOT`
/// or cwd-relative branch here.
pub(crate) fn fragment_path(home: &Path) -> PathBuf {
    home.join(FRAGMENT_RELATIVE_PATH)
}

/// Read the fragment, or `None` if it is missing, unreadable, empty, or over
/// [`MAX_FRAGMENT_BYTES`].
///
/// Every `None` path warns once and the launch proceeds without the flag. There
/// is no failure mode in which this feature prevents a launch.
pub(crate) fn load_fragment(path: &Path) -> Option<String> {
    use std::io::Read;

    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(err) => {
            // Missing is the ordinary case on a legacy install or a first run
            // before staging, so it is debug rather than warn. The launch
            // proceeds either way.
            tracing::debug!(
                path = %path.display(),
                %err,
                "no amplihack system-prompt fragment; launching without it"
            );
            return None;
        }
    };

    // SEC-6: the cap is enforced by the read itself, not by a prior `stat`.
    // `metadata().len()` and a subsequent read are two syscalls against a path,
    // and nothing holds them together: `stat` answers for the file that was
    // there, the read takes the file that is there now. A FIFO collapses that
    // gap from a race to a certainty — it reports length 0 and then delivers
    // however many bytes the writer sends. Read one byte past the cap and
    // refuse anything that reaches it; the extra byte is what distinguishes
    // "exactly at the cap" from "at least one byte over".
    let mut buf = Vec::new();
    if let Err(err) = file.take(MAX_FRAGMENT_BYTES + 1).read_to_end(&mut buf) {
        tracing::warn!(
            path = %path.display(),
            %err,
            "could not read the system-prompt fragment; launching without it"
        );
        return None;
    }
    if buf.len() as u64 > MAX_FRAGMENT_BYTES {
        tracing::warn!(
            path = %path.display(),
            max = MAX_FRAGMENT_BYTES,
            "system-prompt fragment is over the size cap; launching without it"
        );
        return None;
    }
    let text = match String::from_utf8(buf) {
        Ok(text) => text,
        Err(err) => {
            tracing::warn!(
                path = %path.display(),
                %err,
                "system-prompt fragment is not valid UTF-8; launching without it"
            );
            return None;
        }
    };
    if text.trim().is_empty() {
        tracing::warn!(
            path = %path.display(),
            "system-prompt fragment is empty; launching without it"
        );
        return None;
    }
    Some(text)
}

/// Read the installed fragment for this user, or `None` with a warning.
///
/// SEC-1: `$HOME` only. There is deliberately no `AmplihackPaths::resolve_framework_file`
/// call here — that resolver walks UP from the current directory first, so
/// `git clone <repo> && cd repo && amplihack claude` would read an
/// attacker-authored file directly and hand it to the agent at system-prompt
/// privilege, and that file would inherit this fragment's own framing
/// ("supersedes any earlier instruction", naming the guardrails it overrides)
/// for free.
///
/// # What this does NOT close (F-S6)
///
/// **The read path only.** The write path is a separate, larger, pre-existing
/// exposure and this comment used to imply it was covered. It is not:
/// `install::ensure_framework_installed` fires whenever an essential path is
/// missing, and `clone.rs`'s `find_bundled_framework_root` finds its source by
/// walking UP from `current_dir()` — the same cwd-derived channel. It stages
/// `amplifier-bundle/context/` to
/// `$HOME/.amplihack/.claude/context/`, byte-identical to
/// [`FRAGMENT_RELATIVE_PATH`], so the file this function reads can be written
/// by the repo you happen to be standing in.
///
/// That channel is not new and is not this feature's: the same restage already
/// delivers `amplifier-bundle/agents/` (agent instructions) and
/// `tools/amplihack/*.sh` (shell scripts) to the same destination, so it
/// already carries code-execution and agent-instruction authority. Adding one
/// more file is a marginal escalation of an existing exposure, not a new one —
/// which is why it is filed as its own issue (install-source trust model:
/// compiled-in `include_str!`, or restricting source roots to the binary's own
/// origin) rather than patched here. Paired with it: this fragment has no
/// integrity check at read time (A-8) — `load_fragment` opens and trusts, and
/// the restage fires only on a *missing* file, never a modified one.
///
/// Do not read the paragraph above as "so SEC-1 does not matter". It removes
/// the direct cwd read, which is the cheapest half of the chain and the only
/// half this module owns.
pub(crate) fn installed_fragment() -> Option<String> {
    let home = std::env::var_os("HOME")
        .or_else(|| std::env::var_os("USERPROFILE"))
        .map(PathBuf::from)?;
    load_fragment(&fragment_path(&home))
}
