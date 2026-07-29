//! Dimensions 1-4: GitHub Actions security checks.
//!
//! - Dim 1: Action SHA pinning
//! - Dim 2: Workflow permissions hardening
//! - Dim 3: Secret exposure detection
//! - Dim 4: Cache poisoning risk

use super::utils::{Counters, build, load_workflows, mk, relative_path};
use crate::schema::{Finding, Severity};
use once_cell::sync::Lazy;
use regex::Regex;
use std::path::Path;

static SHA_PATTERN: Lazy<Regex> = Lazy::new(|| Regex::new(r"^[0-9a-f]{40}$").unwrap());
static LINE_USES_PATTERN: Lazy<Regex> =
    Lazy::new(|| Regex::new(r#"^\s*-?\s*uses:\s*(.+?)@([^\s#"'\n]+)(.*)$"#).unwrap());

fn has_prt(content: &str) -> bool {
    content.contains("pull_request_target")
}

// ─── Dimension 1: Action SHA Pinning ─────────────────────────────────────────

/// Dim 1: detect action refs not pinned to a full 40-char commit SHA.
pub fn check_action_sha_pinning(root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut counters = Counters::new();

    for (wf_path, content) in load_workflows(root) {
        let rel = relative_path(root, &wf_path);
        let prt = has_prt(&content);

        for (idx, line) in content.lines().enumerate() {
            let line_no = (idx + 1) as i64;
            let Some(caps) = LINE_USES_PATTERN.captures(line) else {
                continue;
            };
            let action_ref = caps.get(1).map_or("", |m| m.as_str()).trim();
            let git_ref = caps.get(2).map_or("", |m| m.as_str()).trim();
            let rest = caps.get(3).map_or("", |m| m.as_str());

            if action_ref.starts_with("./") {
                continue;
            }

            if SHA_PATTERN.is_match(git_ref) {
                let has_comment = rest.contains('#')
                    && rest
                        .split_once('#')
                        .map(|x| x.1)
                        .is_some_and(|c| c.chars().any(|ch| ch.is_alphanumeric()));
                if !has_comment {
                    findings.push(build(mk(
                        &mut counters,
                        Severity::Info,
                        1,
                        &rel,
                        line_no,
                        format!("{action_ref}@{git_ref}"),
                        format!("{action_ref}@{git_ref}  # vX.Y.Z"),
                        "SHA-pinned action missing human-readable version comment.",
                    )));
                }
                continue;
            }

            let severity = if prt {
                Severity::Critical
            } else {
                Severity::High
            };
            let parts: Vec<&str> = action_ref.split('/').collect();
            let fix_url = if parts.len() >= 2 {
                format!(
                    "https://github.com/{}/{}/releases/tag/{}",
                    parts[0], parts[1], git_ref
                )
            } else {
                format!("https://github.com/{action_ref}")
            };

            findings.push(build(
                mk(
                    &mut counters,
                    severity,
                    1,
                    &rel,
                    line_no,
                    format!("{action_ref}@{git_ref}"),
                    format!("{action_ref}@<full-40-char-sha>  # {git_ref}"),
                    format!(
                        "Mutable ref '{git_ref}' allows silent code replacement. \
                         Pin to full commit SHA."
                    ),
                )
                .fix_url(fix_url),
            ));
        }
    }

    findings
}

// ─── Dimension 2: Workflow Permissions ───────────────────────────────────────

static PERM_TOP: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?m)^permissions\s*:").unwrap());
static PERM_WRITE_ALL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"permissions\s*:\s*write-all").unwrap());
static PERM_READ_ALL: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"permissions\s*:\s*read-all").unwrap());
static PERM_EMPTY: Lazy<Regex> = Lazy::new(|| Regex::new(r"permissions\s*:\s*\{\}").unwrap());
static PERM_NONE: Lazy<Regex> = Lazy::new(|| Regex::new(r"permissions\s*:\s*none").unwrap());
static WRITE_SCOPE: Lazy<Regex> = Lazy::new(|| Regex::new(r":\s*write\b").unwrap());
static PERM_LINE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^permissions\s*:").unwrap());
static ON_LINE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^on\s*:|^on\s*$").unwrap());

/// Dim 2: detect missing or over-broad workflow permissions.
pub fn check_workflow_permissions(root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut counters = Counters::new();

    for (wf_path, content) in load_workflows(root) {
        let rel = relative_path(root, &wf_path);
        let prt = has_prt(&content);

        let has_top = PERM_TOP.is_match(&content);
        let write_all = PERM_WRITE_ALL.is_match(&content);
        let read_all = PERM_READ_ALL.is_match(&content);
        let empty = PERM_EMPTY.is_match(&content);
        let none = PERM_NONE.is_match(&content);

        let lines: Vec<&str> = content.lines().collect();
        let perm_line = lines
            .iter()
            .position(|l| PERM_LINE.is_match(l))
            .map_or(1, |i| i + 1) as i64;

        if write_all {
            findings.push(build(mk(
                &mut counters,
                Severity::High,
                2,
                &rel,
                perm_line,
                "permissions: write-all",
                "permissions: read-all",
                "write-all grants GITHUB_TOKEN write access to all scopes. \
                 Use least-privilege: declare only required scopes.",
            )));
        } else if !has_top {
            let severity = if prt {
                Severity::Critical
            } else {
                Severity::High
            };
            let trigger_line = lines
                .iter()
                .position(|l| ON_LINE.is_match(l))
                .map_or(1, |i| i + 1) as i64;
            let current_val = if prt {
                "pull_request_target (no permissions: key)"
            } else {
                "on: [push] (no permissions: key)"
            };
            findings.push(build(mk(
                &mut counters,
                severity,
                2,
                &rel,
                trigger_line,
                current_val,
                "permissions: read-all  # Add top-level permissions",
                "Workflow has no permissions key; GITHUB_TOKEN defaults to \
                 implicit permissions that may include write access.",
            )));
            findings.push(build(mk(
                &mut counters,
                Severity::Medium,
                2,
                &rel,
                1,
                "(no job-level permissions defined)",
                "jobs.<name>.permissions: {}  # restrict per-job",
                "No job-level permissions override. Declare `permissions: {}` \
                 per job for least-privilege across all jobs.",
            )));
        } else if has_top && !read_all && !empty && !none && WRITE_SCOPE.is_match(&content) {
            findings.push(build(mk(
                &mut counters,
                Severity::Medium,
                2,
                &rel,
                perm_line,
                "permissions: (includes write scope)",
                "Minimize write permissions; use id-token: write only where needed",
                "Workflow has write permissions. Verify each scope is necessary \
                 and restrict to minimum required.",
            )));
        }
    }

    findings
}

// ─── Dimension 3: Secret Exposure ────────────────────────────────────────────

static ECHO_PRINT: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)(echo|print|printf|cat|curl|wget|python\s+-c)[^\n]*\$\{\{\s*secrets\.")
        .unwrap()
});
static SECRET_IN_CACHE_KEY: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"(?i)key:\s*.*\$\{\{\s*secrets\.").unwrap());
static SECRET_REF: Lazy<Regex> =
    Lazy::new(|| Regex::new(r"\$\{\{\s*secrets\.(\w+)\s*\}\}").unwrap());

fn secret_name(line: &str) -> String {
    SECRET_REF
        .captures(line)
        .and_then(|c| c.get(1))
        .map_or_else(|| "UNKNOWN".to_string(), |m| m.as_str().to_string())
}

/// Dim 3: detect secrets echoed to logs or embedded in cache keys.
pub fn check_secret_exposure(root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut counters = Counters::new();

    for (wf_path, content) in load_workflows(root) {
        let rel = relative_path(root, &wf_path);
        for (idx, line) in content.lines().enumerate() {
            let line_no = (idx + 1) as i64;
            if ECHO_PRINT.is_match(line) {
                let name = secret_name(line);
                findings.push(build(
                    mk(
                        &mut counters,
                        Severity::Critical,
                        3,
                        &rel,
                        line_no,
                        line.trim(),
                        format!(
                            "Remove echo of secrets.{name}. \
                             Pass secrets only via env: or action with: blocks."
                        ),
                        format!(
                            "Secret 'secrets.{name}' echoed to stdout. \
                             GitHub masks known secrets but value may appear in logs."
                        ),
                    )
                    .contains_secret(true),
                ));
                continue;
            }
            if SECRET_IN_CACHE_KEY.is_match(line) {
                let name = secret_name(line);
                findings.push(build(
                    mk(
                        &mut counters,
                        Severity::High,
                        3,
                        &rel,
                        line_no,
                        line.trim(),
                        "Remove secrets from cache keys. Use hash of lock files instead.",
                        format!(
                            "Secret 'secrets.{name}' in cache key may appear \
                             in cache entry metadata visible to pull request forks."
                        ),
                    )
                    .contains_secret(true),
                ));
            }
        }
    }

    findings
}

// ─── Dimension 4: Cache Poisoning ────────────────────────────────────────────

static CACHE_ACTION: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)uses:\s*actions/cache@").unwrap());
static HASH_IN_KEY: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)hashFiles\s*\(").unwrap());
static KEY_LINE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*key\s*:").unwrap());
static LOWER_LINE: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*[a-z]").unwrap());
static WITH_RUN_USES: Lazy<Regex> = Lazy::new(|| Regex::new(r"^\s*(with|run|uses)").unwrap());

/// Dim 4: detect cache keys susceptible to poisoning (no `hashFiles()`).
pub fn check_cache_poisoning(root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut counters = Counters::new();

    for (wf_path, content) in load_workflows(root) {
        let rel = relative_path(root, &wf_path);
        let mut in_cache_step = false;

        for (idx, line) in content.lines().enumerate() {
            let line_no = (idx + 1) as i64;
            if CACHE_ACTION.is_match(line) {
                in_cache_step = true;
                continue;
            }
            if in_cache_step {
                if KEY_LINE.is_match(line) {
                    if !HASH_IN_KEY.is_match(line) {
                        findings.push(build(mk(
                            &mut counters,
                            Severity::Medium,
                            4,
                            &rel,
                            line_no,
                            line.trim(),
                            "key: ${{ runner.os }}-pip-${{ hashFiles('**/requirements*.txt') }}",
                            "Cache key without hashFiles() is mutable and may serve \
                             poisoned cache entries to subsequent runs.",
                        )));
                    }
                } else if LOWER_LINE.is_match(line) && !WITH_RUN_USES.is_match(line) {
                    in_cache_step = false;
                }
            }
        }
    }

    findings
}
