//! Regression + validation guard for the `agentic-workflow-first` skill: it must
//! remain present and valid in the bundled skill corpus so Copilot CLI keeps
//! listing it and surfacing it on design / "how should I build this" questions.
//!
//! # Background
//!
//! `agentic-workflow-first` is an overwhelmingly-markdown skill that teaches the
//! ordered decision procedure for classifying each step of new work as agentic
//! (judgment → prompt), tool (side-effect → bash), or thin rail (glue → code).
//! Because the skills directory is the single source of truth for Copilot
//! discovery (#865), a skill silently vanishes from the list if its definition
//! is removed, renamed, has frontmatter that does not start at byte 0, carries a
//! non-string scalar field (#890), or has a `name` that no longer matches its
//! directory (#860).
//!
//! This guard fails loudly the moment any of those failure modes appears.
//!
//! # Read-only invariant
//!
//! This test only reads the bundled source files. It never writes or stages.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

use serde_yaml::Value;

const SKILL_NAME: &str = "agentic-workflow-first";

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

fn skill_dir() -> PathBuf {
    workspace_root().join(format!("amplifier-bundle/skills/{SKILL_NAME}"))
}

fn skill_md() -> PathBuf {
    skill_dir().join("SKILL.md")
}

/// Extract the YAML frontmatter block (between the opening `---\n` at byte 0 and
/// the next `\n---`). Returns `None` when frontmatter is absent.
fn extract_frontmatter(content: &str) -> Option<&str> {
    let after_open = content.strip_prefix("---\n")?;
    let close_idx = after_open.find("\n---")?;
    Some(&after_open[..close_idx])
}

/// Parse `SKILL.md`, extract its YAML frontmatter, and return the `name` field as
/// a `String`. Uses `serde_yaml` (like TC-AWF-03) so the name is read from parsed
/// YAML rather than by brittle line scanning, which would mishandle quoting,
/// inline comments, or block scalars.
fn frontmatter_name() -> String {
    let content = fs::read_to_string(skill_md()).expect("read agentic-workflow-first SKILL.md");
    let frontmatter = extract_frontmatter(&content).expect("SKILL.md must have YAML frontmatter");
    let mapping = match serde_yaml::from_str::<Value>(frontmatter) {
        Ok(Value::Mapping(map)) => map,
        Ok(other) => panic!("{SKILL_NAME} frontmatter must be a YAML mapping, found {other:?}"),
        Err(err) => panic!("{SKILL_NAME} frontmatter is not valid YAML: {err}"),
    };
    mapping
        .get("name")
        .and_then(Value::as_str)
        .expect("SKILL.md must declare a string `name` field")
        .to_string()
}

/// TC-AWF-01: The `agentic-workflow-first` skill directory and its canonical
/// `SKILL.md` entrypoint must exist. A missing/moved/renamed definition is the
/// most direct way the skill drops out of Copilot's list.
#[test]
fn tc_awf_01_skill_definition_exists() {
    let dir = skill_dir();
    assert!(
        dir.is_dir(),
        "{SKILL_NAME} skill directory is missing at {} — the skill will not be \
         staged to ~/.copilot/skills and disappears from Copilot CLI",
        dir.display()
    );

    let md = skill_md();
    assert!(
        md.is_file(),
        "{SKILL_NAME}/SKILL.md is missing at {} — Copilot skill discovery requires \
         a canonical uppercase SKILL.md entrypoint",
        md.display()
    );
}

/// TC-AWF-02: `SKILL.md` frontmatter must start at byte 0. A Markdown title or
/// comment before the opening `---` prevents Copilot from parsing the metadata
/// and silently hides the skill.
#[test]
fn tc_awf_02_frontmatter_starts_at_first_byte() {
    let content = fs::read_to_string(skill_md()).expect("read agentic-workflow-first SKILL.md");
    assert!(
        content.starts_with("---\n"),
        "{SKILL_NAME}/SKILL.md frontmatter must start at the first byte"
    );
}

/// TC-AWF-03: `SKILL.md` frontmatter must be valid YAML declaring a string
/// `name: agentic-workflow-first` and a non-empty string `description`.
/// Non-string scalar fields (list/map) are exactly what Copilot CLI rejects
/// (#890), and a wrong name breaks the directory/name match Copilot relies on.
#[test]
fn tc_awf_03_frontmatter_metadata_is_valid() {
    let content = fs::read_to_string(skill_md()).expect("read agentic-workflow-first SKILL.md");
    let frontmatter = extract_frontmatter(&content).expect("SKILL.md must have YAML frontmatter");

    let mapping = match serde_yaml::from_str::<Value>(frontmatter) {
        Ok(Value::Mapping(map)) => map,
        Ok(other) => panic!("{SKILL_NAME} frontmatter must be a YAML mapping, found {other:?}"),
        Err(err) => panic!("{SKILL_NAME} frontmatter is not valid YAML: {err}"),
    };

    let name = mapping
        .get("name")
        .expect("SKILL.md must declare a `name` field");
    assert_eq!(
        name.as_str(),
        Some(SKILL_NAME),
        "{SKILL_NAME}/SKILL.md `name` must be the string \"{SKILL_NAME}\" (must also \
         match the containing directory name for Copilot discovery)"
    );

    let description = mapping
        .get("description")
        .expect("SKILL.md must declare a `description` field");
    match description {
        Value::String(s) => assert!(
            !s.trim().is_empty(),
            "{SKILL_NAME}/SKILL.md `description` must be a non-empty string scalar"
        ),
        other => panic!(
            "{SKILL_NAME}/SKILL.md `description` must be a string scalar — Copilot CLI \
             rejects list/map scalar fields, found {other:?}"
        ),
    }
}

/// TC-AWF-04: The skill name must match its containing directory. Copilot stages
/// each skill into `~/.copilot/skills/<dir>/` and lists it under that directory;
/// a name/dir mismatch makes the listing inconsistent (#860).
#[test]
fn tc_awf_04_name_matches_directory() {
    let name = frontmatter_name();

    let dir_name = skill_dir()
        .file_name()
        .and_then(|n| n.to_str())
        .expect("skill directory has a name")
        .to_string();

    assert_eq!(
        name, dir_name,
        "{SKILL_NAME} skill `name` ({name}) must match its directory ({dir_name})"
    );
}

/// TC-AWF-05: The skill `name` must not contain the forbidden token "Bridge"
/// (case-insensitive) — an explicit naming constraint for this skill.
#[test]
fn tc_awf_05_name_has_no_forbidden_token() {
    let name = frontmatter_name();

    assert!(
        !name.to_lowercase().contains("bridge"),
        "{SKILL_NAME} skill `name` ({name}) must not contain the token \"Bridge\""
    );
}
