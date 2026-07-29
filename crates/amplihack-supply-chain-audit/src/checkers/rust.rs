//! Dimension 9: Rust / Cargo supply chain security checks.

use super::utils::{Counters, build, mk, relative_path};
use crate::schema::{Finding, Severity};
use once_cell::sync::Lazy;
use regex::Regex;
use std::path::Path;

static PATCH_SECTION: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?s)\[patch\.[^\]]+\](.*?)(?:\n\[|\z)").unwrap());
static GIT_BRANCH: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"(?s)git\s*=\s*["'](.+?)["'].*branch\s*=\s*["'](.+?)["']"#).unwrap());
static BRANCH_REF: Lazy<Regex> = Lazy::new(|| Regex::new(r"branch\s*=").unwrap());

/// Dim 9: check Cargo.lock commit status, build.rs risks, and `[patch]` overrides.
pub fn check_cargo_supply_chain(root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut counters = Counters::new();

    let cargo_toml = root.join("Cargo.toml");
    if !cargo_toml.exists() {
        return findings;
    }
    let rel_cargo = relative_path(root, &cargo_toml);
    let Ok(cargo_content) = std::fs::read_to_string(&cargo_toml) else {
        return findings;
    };

    let is_binary = cargo_content.contains("[[bin]]") || root.join("src").join("main.rs").exists();

    if is_binary {
        let gitignore = root.join(".gitignore");
        let mut cargo_lock_ignored = false;
        if let Ok(gi) = std::fs::read_to_string(&gitignore) {
            cargo_lock_ignored = gi
                .lines()
                .map(str::trim)
                .any(|l| l == "Cargo.lock" || l == "/Cargo.lock");
        }
        if cargo_lock_ignored {
            findings.push(build(mk(
                &mut counters,
                Severity::Medium,
                9,
                ".gitignore",
                0,
                "Cargo.lock (binary crate with Cargo.lock in .gitignore)",
                "Remove Cargo.lock from .gitignore for binary crates. \
                 Commit Cargo.lock to ensure reproducible builds.",
                "Binary crates should commit Cargo.lock to guarantee reproducible builds. \
                 Excluding it allows crates.io resolution to silently use newer (possibly \
                 compromised) dependency versions.",
            )));
        }
    }

    if let Some(patch_caps) = PATCH_SECTION.captures(&cargo_content) {
        let patch_section = patch_caps.get(0).map_or("", |m| m.as_str());
        let patch_start = cargo_content.find("[patch.").unwrap_or(0);
        let line_no = (cargo_content[..patch_start].matches('\n').count() + 1) as i64;

        if let Some(gb) = GIT_BRANCH.captures(patch_section) {
            let repo_url = gb.get(1).map_or("", |m| m.as_str());
            let branch = gb.get(2).map_or("", |m| m.as_str());
            findings.push(build(mk(
                &mut counters,
                Severity::Medium,
                9,
                &rel_cargo,
                line_no,
                format!("[patch] using git = '{repo_url}' branch = '{branch}'"),
                format!("[patch] using git = '{repo_url}' rev = '<full-commit-sha>'"),
                format!(
                    "[patch] with branch = '{branch}' is mutable — the branch can be \
                     force-pushed, silently changing the resolved dependency. \
                     Pin to a specific commit SHA with rev = '...'."
                ),
            )));
        } else if BRANCH_REF.is_match(patch_section) {
            findings.push(build(mk(
                &mut counters,
                Severity::Medium,
                9,
                &rel_cargo,
                line_no,
                "[patch] section uses mutable branch reference",
                "[patch] should use rev = '<commit-sha>' for reproducibility",
                "[patch] with branch reference is mutable. \
                 Pin to a specific commit rev for reproducible builds.",
            )));
        }
    }

    let build_rs = root.join("build.rs");
    if build_rs.exists() {
        let rel_build = relative_path(root, &build_rs);
        findings.push(build(mk(
            &mut counters,
            Severity::Info,
            9,
            &rel_build,
            0,
            "build.rs present",
            "Review build.rs for network calls, arbitrary code execution, \
             or env variable leakage",
            "build.rs runs arbitrary Rust code at compile time. Review for network calls, \
             file system access outside project, or environment variable leakage.",
        )));
    }

    findings
}
