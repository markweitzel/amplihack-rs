//! Dimension 8: Python dependency integrity checks.

use super::utils::{Counters, build, load_workflows, mk, relative_path};
use crate::schema::{Finding, Severity};
use once_cell::sync::Lazy;
use regex::Regex;
use std::path::Path;

static PIP_INSTALL_REQ: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"pip\s+install\s+(-r\s+\S+|-r\S+)").unwrap());

/// Dim 8: detect missing hash pinning and dependency-confusion vectors.
pub fn check_python_integrity(root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut counters = Counters::new();

    for fname in [
        "requirements.txt",
        "requirements-dev.txt",
        "requirements-test.txt",
    ] {
        let req_file = root.join(fname);
        if !req_file.exists() {
            continue;
        }
        let rel = relative_path(root, &req_file);
        let Ok(content) = std::fs::read_to_string(&req_file) else {
            continue;
        };
        let lines: Vec<&str> = content.lines().collect();

        for (idx, line) in lines.iter().enumerate() {
            let stripped = line.trim();
            if stripped.starts_with("--extra-index-url") {
                findings.push(build(mk(
                    &mut counters,
                    Severity::High,
                    8,
                    &rel,
                    (idx + 1) as i64,
                    stripped,
                    "Use --index-url (single source) or configure package source mapping. \
                     Avoid --extra-index-url which enables dependency confusion.",
                    "--extra-index-url enables dependency confusion: an attacker can \
                     publish a higher-versioned package on PyPI to override your internal package.",
                )));
            }
        }

        let has_hash = content.contains("--hash=");
        let has_package = lines.iter().any(|l| {
            let s = l.trim();
            !s.is_empty() && !s.starts_with('#') && !s.starts_with('-')
        });

        if has_package && !has_hash {
            findings.push(build(mk(
                &mut counters,
                Severity::High,
                8,
                &rel,
                0,
                format!("{rel} has no --hash= annotations"),
                "Use pip-compile --generate-hashes or add --hash=sha256:... to each package. \
                 Run: pip install --require-hashes -r requirements.txt",
                "Without hash pinning, pip accepts any package matching the version \
                 specifier, allowing silent substitution by a compromised PyPI mirror.",
            )));
        }
    }

    for (wf_path, content) in load_workflows(root) {
        let rel = relative_path(root, &wf_path);
        for (idx, line) in content.lines().enumerate() {
            let stripped = line.trim();
            if PIP_INSTALL_REQ.is_match(stripped)
                && !stripped.contains("--require-hashes")
                && !stripped.contains("install --require-hashes")
            {
                findings.push(build(mk(
                    &mut counters,
                    Severity::Medium,
                    8,
                    &rel,
                    (idx + 1) as i64,
                    stripped,
                    stripped.replace("pip install", "pip install --require-hashes"),
                    "pip install without --require-hashes does not verify package integrity \
                     even if requirements.txt contains hash annotations.",
                )));
                break;
            }
        }
    }

    findings
}
