//! Regression guard for issue #860: the `pr-guide` skill must remain present
//! and valid in the bundled skill corpus so Copilot CLI keeps listing it.
//!
//! # Background
//!
//! `pr-guide` previously disappeared from the Copilot CLI skills list because a
//! hardcoded skill registry (`crates/amplihack-hooks/src/known_skills.rs`)
//! gated which skills were exposed and did not include it. That registry was
//! patched (#821) and then removed entirely (#865: "skills directory is the
//! single source of truth"), so the Copilot path is now filesystem-driven.
//!
//! Nothing, however, pinned `pr-guide` specifically. This guard fails loudly
//! the moment the `pr-guide` skill definition is removed, renamed, or given
//! malformed metadata — the concrete failure modes that make a skill vanish
//! from Copilot's list.
//!
//! # Read-only invariant
//!
//! This test only reads the bundled source files. It never writes or stages.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_yaml::Value;

/// Walk up from the crate manifest dir until we find the workspace root
/// (the directory that owns both `amplifier-bundle/` and `Cargo.toml`).
fn workspace_root() -> &'static Path {
    static ROOT: OnceLock<PathBuf> = OnceLock::new();
    ROOT.get_or_init(|| {
        let mut cur = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
        loop {
            if cur.join("amplifier-bundle").is_dir() && cur.join("Cargo.toml").is_file() {
                return cur;
            }
            assert!(
                cur.pop(),
                "walked above filesystem root looking for workspace root"
            );
        }
    })
    .as_path()
}

fn pr_guide_dir() -> PathBuf {
    workspace_root().join("amplifier-bundle/skills/pr-guide")
}

fn pr_guide_skill_md() -> PathBuf {
    pr_guide_dir().join("SKILL.md")
}

/// Extract the YAML frontmatter block (between the opening `---\n` at byte 0 and
/// the next `\n---`). Returns `None` when frontmatter is absent.
fn extract_frontmatter(content: &str) -> Option<&str> {
    let after_open = content.strip_prefix("---\n")?;
    let close_idx = after_open.find("\n---")?;
    Some(&after_open[..close_idx])
}

/// TC-860-01: The `pr-guide` skill directory and its canonical `SKILL.md`
/// entrypoint must exist. A missing/moved/renamed definition is the most direct
/// way the skill drops out of Copilot's list.
#[test]
fn tc_860_01_pr_guide_skill_definition_exists() {
    let dir = pr_guide_dir();
    assert!(
        dir.is_dir(),
        "pr-guide skill directory is missing at {} — the skill will not be \
         staged to ~/.copilot/skills and disappears from Copilot CLI",
        dir.display()
    );

    let skill_md = pr_guide_skill_md();
    assert!(
        skill_md.is_file(),
        "pr-guide/SKILL.md is missing at {} — Copilot skill discovery requires a \
         canonical uppercase SKILL.md entrypoint",
        skill_md.display()
    );
}

/// TC-860-02: `pr-guide/SKILL.md` frontmatter must start at byte 0. A Markdown
/// title or comment before the opening `---` prevents Copilot from parsing the
/// metadata and silently hides the skill.
#[test]
fn tc_860_02_pr_guide_frontmatter_starts_at_first_byte() {
    let content = fs::read_to_string(pr_guide_skill_md()).expect("read pr-guide SKILL.md");
    assert!(
        content.starts_with("---\n"),
        "pr-guide/SKILL.md frontmatter must start at the first byte"
    );
}

/// TC-860-03: `pr-guide/SKILL.md` frontmatter must be valid YAML declaring a
/// string `name: pr-guide` and a string `description`. Non-string scalar fields
/// (list/map) are exactly what Copilot CLI rejects (issue #890), and a wrong
/// name breaks the directory/name match Copilot relies on.
#[test]
fn tc_860_03_pr_guide_frontmatter_metadata_is_valid() {
    let content = fs::read_to_string(pr_guide_skill_md()).expect("read pr-guide SKILL.md");
    let frontmatter =
        extract_frontmatter(&content).expect("pr-guide/SKILL.md must have YAML frontmatter");

    let mapping = match serde_yaml::from_str::<Value>(frontmatter) {
        Ok(Value::Mapping(map)) => map,
        Ok(other) => panic!("pr-guide frontmatter must be a YAML mapping, found {other:?}"),
        Err(err) => panic!("pr-guide frontmatter is not valid YAML: {err}"),
    };

    let name = mapping
        .get("name")
        .expect("pr-guide/SKILL.md must declare a `name` field");
    assert_eq!(
        name.as_str(),
        Some("pr-guide"),
        "pr-guide/SKILL.md `name` must be the string \"pr-guide\" (must also \
         match the containing directory name for Copilot discovery)"
    );

    let description = mapping
        .get("description")
        .expect("pr-guide/SKILL.md must declare a `description` field");
    assert!(
        matches!(description, Value::String(_)),
        "pr-guide/SKILL.md `description` must be a string scalar — Copilot CLI \
         rejects list/map scalar fields"
    );
}

/// TC-860-04: The `pr-guide` skill name must match its containing directory.
/// Copilot stages each skill into `~/.copilot/skills/<dir>/` and lists it under
/// that directory; a name/dir mismatch makes the listing inconsistent.
#[test]
fn tc_860_04_pr_guide_name_matches_directory() {
    let content = fs::read_to_string(pr_guide_skill_md()).expect("read pr-guide SKILL.md");
    let frontmatter =
        extract_frontmatter(&content).expect("pr-guide/SKILL.md must have YAML frontmatter");
    let name = frontmatter
        .lines()
        .find_map(|line| line.trim().strip_prefix("name:"))
        .map(str::trim)
        .expect("pr-guide/SKILL.md must declare a `name` field");

    let dir_name = pr_guide_dir()
        .file_name()
        .and_then(|n| n.to_str())
        .expect("pr-guide directory has a name")
        .to_string();

    assert_eq!(
        name, dir_name,
        "pr-guide skill `name` ({name}) must match its directory ({dir_name})"
    );
}
