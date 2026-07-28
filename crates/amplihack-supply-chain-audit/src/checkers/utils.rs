//! Shared helpers for the per-dimension checkers.

use crate::schema::{Finding, FindingBuilder, Severity};
use std::path::{Path, PathBuf};
use walkdir::{DirEntry, WalkDir};

/// True for the `.git` VCS metadata directory, which is pruned from recursive
/// repo scans: it never holds working-tree project files (`*.csproj`,
/// `Dockerfile`, ...) yet can contain 100k+ object files on real repos.
fn is_vcs_metadata_dir(entry: &DirEntry) -> bool {
    entry.file_type().is_dir() && entry.file_name() == ".git"
}

/// Recursively walk `root`, skipping the `.git` directory. Yields entries in
/// the same order as a plain [`WalkDir`] traversal (minus `.git`), so results
/// are identical to an unpruned scan while avoiding wasted VCS traversal.
pub(crate) fn walk_repo(root: &Path) -> impl Iterator<Item = DirEntry> {
    WalkDir::new(root)
        .into_iter()
        .filter_entry(|e| !is_vcs_metadata_dir(e))
        .flatten()
}

/// POSIX-style relative path of `path` under `root` (falls back to the full
/// path when `path` is not nested under `root`).
pub(crate) fn relative_path(root: &Path, path: &Path) -> String {
    match path.strip_prefix(root) {
        Ok(rel) => rel.to_string_lossy().replace('\\', "/"),
        Err(_) => path.to_string_lossy().replace('\\', "/"),
    }
}

/// True for gh-aw rendered template lock files (`*.lock.yml`), which must be
/// excluded from workflow auditing to avoid false positives on prompt content.
pub(crate) fn is_lock_file(path: &Path) -> bool {
    path.file_name()
        .and_then(|n| n.to_str())
        .is_some_and(|n| n.contains(".lock."))
}

/// Load all `.github/workflows/*.{yml,yaml}` files (sorted, lock files skipped).
pub(crate) fn load_workflows(root: &Path) -> Vec<(PathBuf, String)> {
    let wf_dir = root.join(".github").join("workflows");
    let mut paths: Vec<PathBuf> = Vec::new();
    if let Ok(rd) = std::fs::read_dir(&wf_dir) {
        for entry in rd.flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "yml" || e == "yaml") && !is_lock_file(&p) {
                paths.push(p);
            }
        }
    }
    paths.sort();
    paths
        .into_iter()
        .filter_map(|p| std::fs::read_to_string(&p).ok().map(|c| (p, c)))
        .collect()
}

/// Per-severity sequential ID generator (`HIGH-001`, `HIGH-002`, ...).
pub(crate) struct Counters {
    counts: [u32; 4],
}

impl Counters {
    pub(crate) fn new() -> Self {
        Self { counts: [0; 4] }
    }

    /// Next zero-padded ID for the given severity.
    pub(crate) fn next(&mut self, severity: Severity) -> String {
        let idx = severity.rank() as usize;
        self.counts[idx] += 1;
        format!("{}-{:03}", severity.prefix(), self.counts[idx])
    }
}

/// Start a finding builder with a fresh sequential ID. `offline_detectable` is
/// always true for the offline checkers.
#[allow(clippy::too_many_arguments)]
pub(crate) fn mk(
    counters: &mut Counters,
    severity: Severity,
    dimension: u32,
    file: &str,
    line: i64,
    current_value: impl Into<String>,
    expected_value: impl Into<String>,
    rationale: impl Into<String>,
) -> FindingBuilder {
    let id = counters.next(severity);
    Finding::builder(
        id,
        dimension,
        severity,
        file.to_string(),
        line,
        current_value,
        expected_value,
        rationale,
        true,
    )
}

/// Finalize a builder, panicking on internally malformed findings (a bug).
pub(crate) fn build(builder: FindingBuilder) -> Finding {
    builder
        .build()
        .expect("checker produced an invalid finding")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    #[test]
    fn walk_repo_prunes_git_directory() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path();
        fs::create_dir_all(root.join(".git/objects")).unwrap();
        fs::write(root.join(".git/objects/Dockerfile"), "FROM scratch").unwrap();
        fs::write(root.join("Dockerfile"), "FROM scratch").unwrap();

        let names: Vec<String> = walk_repo(root)
            .filter(|e| e.file_name() == "Dockerfile")
            .map(|e| e.path().to_string_lossy().into_owned())
            .collect();

        assert_eq!(
            names.len(),
            1,
            "only the top-level Dockerfile should be seen"
        );
        assert!(names[0].ends_with("Dockerfile"));
        assert!(!names[0].contains(".git"), "the .git tree must be pruned");
    }
}
