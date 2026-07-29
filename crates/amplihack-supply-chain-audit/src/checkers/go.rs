//! Dimension 11: Go module integrity checks.

use super::utils::{Counters, build, load_workflows, mk, relative_path};
use crate::schema::{Finding, Severity};
use once_cell::sync::Lazy;
use regex::Regex;
use std::path::Path;

static REQUIRE_LINE: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^require\s").unwrap());
static SEMVER: Lazy<Regex> = Lazy::new(|| Regex::new(r"^v\d+\.\d+\.\d+").unwrap());
static GONOSUMCHECK: Lazy<Regex> = Lazy::new(|| Regex::new(r"GONOSUMCHECK\s*:").unwrap());
static GONOSUMDB: Lazy<Regex> = Lazy::new(|| Regex::new(r"GONOSUMDB\s*:").unwrap());

/// Dim 11: check `go.sum` presence, mutable `replace` refs, and checksum bypass.
pub fn check_go_module_integrity(root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut counters = Counters::new();

    let go_mod = root.join("go.mod");
    let go_sum = root.join("go.sum");
    if !go_mod.exists() {
        return findings;
    }
    let rel_gomod = relative_path(root, &go_mod);
    let Ok(content) = std::fs::read_to_string(&go_mod) else {
        return findings;
    };

    let has_deps = REQUIRE_LINE.is_match(&content);
    if has_deps && !go_sum.exists() {
        findings.push(build(mk(
            &mut counters,
            Severity::High,
            11,
            &rel_gomod,
            0,
            "go.sum absent (module checksums not committed)",
            "Commit go.sum: run `go mod tidy` and commit the resulting go.sum",
            "go.sum contains cryptographic checksums for all dependencies. \
             Without it, Go cannot verify module integrity on subsequent builds.",
        )));
    }

    for (idx, line) in content.lines().enumerate() {
        let stripped = line.trim();
        if !(stripped.starts_with("replace ") && stripped.contains("=>")) {
            continue;
        }
        let line_no = (idx + 1) as i64;
        let Some((lhs, rhs)) = stripped.split_once("=>") else {
            continue;
        };
        let target = rhs.trim();

        if target.starts_with("./") || target.starts_with("../") {
            findings.push(build(mk(
                &mut counters,
                Severity::Info,
                11,
                &rel_gomod,
                line_no,
                stripped,
                "Ensure local replace path is intentional and documented",
                "replace directive points to a local path. Verify this is intentional \
                 and not accidentally committed.",
            )));
            continue;
        }

        let tokens: Vec<&str> = target.split_whitespace().collect();
        if tokens.len() >= 2 {
            let module_path = tokens[0];
            let version_or_branch = tokens[1];
            let is_semver = SEMVER.is_match(version_or_branch);
            if !is_semver
                && !version_or_branch.is_empty()
                && !version_or_branch.starts_with("v0.0.0")
            {
                findings.push(build(mk(
                    &mut counters,
                    Severity::Medium,
                    11,
                    &rel_gomod,
                    line_no,
                    stripped,
                    format!(
                        "replace {} => {module_path} v0.0.0-<date>-<sha>",
                        lhs.trim()
                    ),
                    format!(
                        "replace directive uses mutable ref '{version_or_branch}'. \
                         Pin to a specific commit SHA using pseudo-version format."
                    ),
                )));
            }
        }
    }

    for (wf_path, wf_content) in load_workflows(root) {
        let rel_wf = relative_path(root, &wf_path);
        for (idx, line) in wf_content.lines().enumerate() {
            if GONOSUMCHECK.is_match(line) || GONOSUMDB.is_match(line) {
                let env_var = if line.contains("GONOSUMCHECK") {
                    "GONOSUMCHECK"
                } else {
                    "GONOSUMDB"
                };
                findings.push(build(mk(
                    &mut counters,
                    Severity::High,
                    11,
                    &rel_wf,
                    (idx + 1) as i64,
                    line.trim(),
                    format!(
                        "Remove {env_var}; use GONOSUMCHECK only for private modules with \
                         GONOSUMCHECK=<specific-module>"
                    ),
                    format!(
                        "{env_var} disables checksum verification for matched modules, \
                         allowing tampered dependencies to be used without detection."
                    ),
                )));
            }
        }
    }

    findings
}
