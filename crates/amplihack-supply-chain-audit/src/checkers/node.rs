//! Dimension 10: Node.js / npm security checks.

use super::utils::{Counters, build, load_workflows, mk, relative_path};
use crate::schema::{Finding, Severity};
use once_cell::sync::Lazy;
use regex::Regex;
use std::path::Path;

static NPX_TOKEN: Lazy<Regex> = Lazy::new(|| Regex::new(r"\bnpx\s+(\S+)").unwrap());
static NPM_INSTALL: Lazy<Regex> = Lazy::new(|| Regex::new(r"\bnpm\s+install\b").unwrap());
static NPM_INSTALL_FLAG: Lazy<Regex> = Lazy::new(|| Regex::new(r"npm\s+install\s+--").unwrap());
static NPM_INSTALL_PKG: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"npm\s+install\s+[a-zA-Z@]").unwrap());

fn npx_pkg_name(full_token: &str) -> Option<String> {
    if full_token.starts_with('.') || full_token.starts_with('/') {
        return None;
    }
    if full_token.contains('@') {
        if full_token.starts_with('@') {
            if let Some(slash_idx) = full_token.find('/') {
                let rest = &full_token[slash_idx + 1..];
                if rest.contains('@') {
                    return None; // scoped package with version
                }
            }
        } else {
            return None; // plain package@version
        }
    }
    let pkg_name = if full_token.starts_with('@') {
        full_token.to_string()
    } else {
        full_token
            .split('@')
            .next()
            .unwrap_or(full_token)
            .to_string()
    };
    if pkg_name.starts_with('-') {
        return None;
    }
    Some(pkg_name)
}

/// Dim 10: check Node.js lock files, `npm ci`, and unversioned `npx` usage.
pub fn check_node_integrity(root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut counters = Counters::new();

    let pkg_json = root.join("package.json");
    let has_pkg_json = pkg_json.exists();
    let has_lock = root.join("package-lock.json").exists()
        || root.join("yarn.lock").exists()
        || root.join("pnpm-lock.yaml").exists();
    let rel_pkg = if has_pkg_json {
        relative_path(root, &pkg_json)
    } else {
        "package.json".to_string()
    };

    if has_pkg_json && !has_lock {
        findings.push(build(mk(
            &mut counters,
            Severity::High,
            10,
            &rel_pkg,
            0,
            "no package-lock.json",
            "Add package-lock.json: run npm install once and commit the lock file",
            "Without a lock file, npm install resolves to the latest compatible version \
             on each run. This allows silent dependency updates that could introduce \
             malicious packages.",
        )));
    }

    if has_pkg_json
        && let Ok(pkg_content) = std::fs::read_to_string(&pkg_json)
        && let Ok(pkg_data) = serde_json::from_str::<serde_json::Value>(&pkg_content)
        && let Some(scripts) = pkg_data.get("scripts").and_then(|s| s.as_object())
    {
        for (script_name, script_cmd) in scripts {
            let Some(cmd) = script_cmd.as_str() else {
                continue;
            };
            for caps in NPX_TOKEN.captures_iter(cmd) {
                let full_token = caps.get(1).map_or("", |m| m.as_str());
                let Some(pkg_name) = npx_pkg_name(full_token) else {
                    continue;
                };
                let line_no = pkg_content
                    .lines()
                    .position(|l| l.contains(script_name) && l.contains(cmd))
                    .map_or(0, |i| i + 1) as i64;
                findings.push(build(mk(
                    &mut counters,
                    Severity::High,
                    10,
                    &rel_pkg,
                    line_no,
                    format!("npx {pkg_name} (unversioned)"),
                    format!("npx {pkg_name}@<version> (pin to specific version)"),
                    format!(
                        "Unversioned `npx {pkg_name}` downloads the latest version at \
                                     runtime, bypassing lock file protections. Pin to a specific \
                                     version."
                    ),
                )));
            }
        }
    }

    for (wf_path, content) in load_workflows(root) {
        let rel_wf = relative_path(root, &wf_path);
        for (idx, line) in content.lines().enumerate() {
            let stripped = line.trim();
            if NPM_INSTALL.is_match(stripped)
                && !NPM_INSTALL_FLAG.is_match(stripped)
                && !NPM_INSTALL_PKG.is_match(stripped)
            {
                findings.push(build(mk(
                    &mut counters,
                    Severity::High,
                    10,
                    &rel_wf,
                    (idx + 1) as i64,
                    stripped,
                    stripped.replace("npm install", "npm ci"),
                    "`npm install` can upgrade packages and modify package-lock.json, \
                     bypassing lock file protections. Use `npm ci` in CI pipelines.",
                )));
                break;
            }
        }
    }

    findings
}
