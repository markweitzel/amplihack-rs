//! Dimensions 5 and 12: container image security checks.
//!
//! - Dim 5: image digest pinning (`FROM` instructions)
//! - Dim 12: build chain integrity (final-stage `USER`)

use super::utils::{Counters, build, mk, relative_path, walk_repo};
use crate::schema::{Finding, Severity};
use once_cell::sync::Lazy;
use regex::Regex;
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

static SHA_DIGEST: Lazy<Regex> = Lazy::new(|| Regex::new(r"^sha256:[a-f0-9]{64}$").unwrap());
static FROM_PATTERN: Lazy<Regex> = Lazy::new(|| {
    Regex::new(r"(?i)^FROM\s+([^\s:@]+)(?::([^\s@]+))?(?:@(sha256:[a-f0-9]+))?(?:\s+AS\s+\w+)?\s*$")
        .unwrap()
});
static FROM_PREFIX: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)^FROM\s+").unwrap());
static USER_INSTRUCTION: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?im)^USER\s+\S+").unwrap());

fn find_dockerfiles(root: &Path) -> Vec<PathBuf> {
    let mut files: BTreeSet<PathBuf> = BTreeSet::new();
    for name in ["Dockerfile", "dockerfile"] {
        let p = root.join(name);
        if p.exists() {
            files.insert(p);
        }
    }
    for entry in walk_repo(root) {
        if entry.file_name() == "Dockerfile" {
            files.insert(entry.path().to_path_buf());
        }
    }
    files.into_iter().collect()
}

// ─── Dimension 5: Container Image Pinning ─────────────────────────────────────

/// Dim 5: detect `FROM` instructions using mutable tags instead of digest pins.
pub fn check_container_image_pinning(root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut counters = Counters::new();

    for df_path in find_dockerfiles(root) {
        let rel = relative_path(root, &df_path);
        let Ok(content) = std::fs::read_to_string(&df_path) else {
            continue;
        };
        for (idx, line) in content.lines().enumerate() {
            let line_no = (idx + 1) as i64;
            let stripped = line.trim();
            let Some(caps) = FROM_PATTERN.captures(stripped) else {
                continue;
            };
            let image = caps.get(1).map_or("", |m| m.as_str());
            let tag = caps.get(2).map_or("", |m| m.as_str());
            let digest = caps.get(3).map_or("", |m| m.as_str());

            if image.eq_ignore_ascii_case("scratch") {
                continue;
            }
            if SHA_DIGEST.is_match(digest) || !digest.is_empty() {
                continue;
            }

            if tag.is_empty() {
                findings.push(build(mk(
                    &mut counters,
                    Severity::Critical,
                    5,
                    &rel,
                    line_no,
                    stripped,
                    format!("FROM {image}@sha256:<digest>  # pin to specific digest"),
                    format!(
                        "Image '{image}' has no tag or digest. \
                         Implicit :latest pulls can silently change the build environment."
                    ),
                )));
            } else if tag.eq_ignore_ascii_case("latest") {
                findings.push(build(mk(
                    &mut counters,
                    Severity::Critical,
                    5,
                    &rel,
                    line_no,
                    stripped,
                    format!("FROM {image}@sha256:<digest>  # pin to specific digest"),
                    "':latest' tag is mutable and changes without notice. \
                     Pin to a specific SHA digest for reproducible builds.",
                )));
            } else {
                findings.push(build(mk(
                    &mut counters,
                    Severity::High,
                    5,
                    &rel,
                    line_no,
                    stripped,
                    format!("FROM {image}@sha256:<digest>  # {tag}"),
                    format!(
                        "Tag '{tag}' is mutable and can be retagged to a different image. \
                         Pin to a specific SHA digest."
                    ),
                )));
            }
        }
    }

    findings
}

// ─── Dimension 12: Docker Build Chain Integrity ───────────────────────────────

/// Dim 12: detect final build stages that run as root (no `USER`).
pub fn check_docker_build_chain(root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut counters = Counters::new();

    for df_path in find_dockerfiles(root) {
        let rel = relative_path(root, &df_path);
        let Ok(content) = std::fs::read_to_string(&df_path) else {
            continue;
        };
        let lines: Vec<&str> = content.lines().collect();

        let mut stage_starts: Vec<usize> = Vec::new();
        for (idx, line) in lines.iter().enumerate() {
            if FROM_PREFIX.is_match(line.trim()) {
                stage_starts.push(idx + 1);
            }
        }

        if let Some(&final_start) = stage_starts.last() {
            let final_section = lines[final_start - 1..].join("\n");
            if !USER_INSTRUCTION.is_match(&final_section) {
                findings.push(build(mk(
                    &mut counters,
                    Severity::High,
                    12,
                    &rel,
                    final_start as i64,
                    format!("Final stage (FROM ... line {final_start}) has no USER instruction"),
                    "Add: RUN addgroup -S appgroup && adduser -S appuser -G appgroup\n\
                     USER appuser",
                    "Final stage runs as root. Container escapes could gain root on host. \
                     Add a non-root USER instruction.",
                )));
            }
        }
    }

    findings
}
