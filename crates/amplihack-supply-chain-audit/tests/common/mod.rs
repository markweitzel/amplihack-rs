//! Shared helpers for the supply-chain-audit test suite.

#![allow(dead_code)]

use std::fs;
use std::path::{Path, PathBuf};
use tempfile::TempDir;

/// Create a fresh temp directory to act as an audit root.
pub fn temp_repo() -> TempDir {
    TempDir::new().expect("create temp dir")
}

/// Write `content` to `root/rel`, creating parent directories as needed.
pub fn write_file(root: &Path, rel: &str, content: &str) -> PathBuf {
    let path = root.join(rel);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(&path, content).expect("write file");
    path
}

/// Directory holding the checked-in scenario fixtures.
pub fn fixtures_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests")
        .join("fixtures")
}

/// Copy a fixture scenario into `dest` and normalise `workflows/` to
/// `.github/workflows/` to simulate a real repository layout. Returns the
/// copied repo root.
pub fn copy_fixture_as_repo(scenario: &str, dest: &Path) -> PathBuf {
    let src = fixtures_root().join(scenario);
    let repo = dest.join(scenario);
    copy_dir_all(&src, &repo);

    let wf_src = repo.join("workflows");
    if wf_src.is_dir() {
        let gha = repo.join(".github").join("workflows");
        fs::create_dir_all(gha.parent().unwrap()).expect("mk .github");
        fs::rename(&wf_src, &gha).expect("rename workflows -> .github/workflows");
    }
    repo
}

fn copy_dir_all(src: &Path, dst: &Path) {
    fs::create_dir_all(dst).expect("create dst dir");
    for entry in fs::read_dir(src).expect("read_dir fixture") {
        let entry = entry.expect("dir entry");
        let ty = entry.file_type().expect("file type");
        let target = dst.join(entry.file_name());
        if ty.is_dir() {
            copy_dir_all(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), &target).expect("copy file");
        }
    }
}
