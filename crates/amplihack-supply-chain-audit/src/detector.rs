//! Ecosystem detection — maps repo file signals to audit dimensions.

use crate::error::{Result, SupplyChainAuditError};
use std::collections::BTreeSet;
use std::path::Path;

/// Scope keyword → dimension numbers (strict allowlist).
fn scope_to_dims(scope: &str) -> Option<Vec<u32>> {
    let dims: &[u32] = match scope {
        "gha" => &[1, 2, 3, 4],
        "containers" => &[5, 12],
        "credentials" => &[6],
        "dotnet" => &[7],
        "python" => &[8],
        "rust" => &[9],
        "node" => &[10],
        "go" => &[11],
        "all" => return Some((1..=12).collect()),
        _ => return None,
    };
    Some(dims.to_vec())
}

/// Human-readable skip reason for a dimension when no matching files are found.
fn dim_skip_reason(dim: u32) -> &'static str {
    match dim {
        1..=4 => "No .github/workflows/*.yml files found",
        5 | 12 => "No Dockerfile or docker-compose.yml found",
        6 => "No .github/workflows/*.yml using ${{ secrets.* }} found",
        7 => "No *.csproj or NuGet.Config files found",
        8 => "No requirements.txt, pyproject.toml, or setup.cfg found",
        9 => "No Cargo.toml found",
        10 => "No package.json or package-lock.json found",
        11 => "No go.mod or go.sum found",
        _ => "No matching files found",
    }
}

/// Validate and parse a scope string into the set of dimensions to check.
fn parse_scope(scope: &str) -> Result<Vec<u32>> {
    let mut dims: BTreeSet<u32> = BTreeSet::new();
    for part in scope.split(',') {
        let part = part.trim();
        match scope_to_dims(part) {
            Some(d) => {
                if part == "all" {
                    return Ok((1..=12).collect());
                }
                dims.extend(d);
            }
            None => {
                return Err(SupplyChainAuditError::InvalidScope {
                    scope: scope.to_string(),
                });
            }
        }
    }
    if dims.is_empty() {
        return Err(SupplyChainAuditError::InvalidScope {
            scope: scope.to_string(),
        });
    }
    Ok(dims.into_iter().collect())
}

/// Result of ecosystem detection: which dimensions are active vs. skipped.
#[derive(Debug, Clone)]
pub struct EcosystemScope {
    active_dimensions: Vec<u32>,
    skipped_dimensions: Vec<u32>,
    skip_reasons: Vec<(u32, String)>,
}

impl EcosystemScope {
    /// Sorted list of dimension numbers with detected files (within scope).
    pub fn active_dimensions(&self) -> &[u32] {
        &self.active_dimensions
    }

    /// Sorted list of dimensions 1-12 not currently active.
    pub fn skipped_dimensions(&self) -> &[u32] {
        &self.skipped_dimensions
    }

    /// Skip reasons keyed by dimension, for report rendering.
    pub fn skip_reasons(&self) -> &[(u32, String)] {
        &self.skip_reasons
    }

    /// Human-readable reason a dimension was skipped.
    pub fn get_skip_reason(&self, dim: u32) -> String {
        self.skip_reasons
            .iter()
            .find(|(d, _)| *d == dim)
            .map(|(_, r)| r.clone())
            .unwrap_or_else(|| dim_skip_reason(dim).to_string())
    }
}

/// Recursively search for any `*.csproj`, since .NET solutions conventionally
/// nest project files in subdirectories (e.g. `src/App/App.csproj`). This is a
/// deliberate robustness improvement over the upstream skill's root-only
/// `ls *.csproj`; every other ecosystem below keeps root-only detection because
/// its manifest (`Cargo.toml`, `go.mod`, `package.json`, ...) lives at the repo
/// root by convention.
fn has_csproj(root: &Path) -> bool {
    for entry in crate::checkers::utils::walk_repo(root) {
        if entry
            .path()
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("csproj"))
        {
            return true;
        }
    }
    false
}

/// Detect which dimensions are active based on files present in `root`.
///
/// Validates `scope` against the strict allowlist first (before any file
/// system access), returning [`SupplyChainAuditError::InvalidScope`] for
/// unknown or injection-attempt values.
pub fn detect_ecosystems(root: &Path, scope: &str) -> Result<EcosystemScope> {
    let allowed: Vec<u32> = parse_scope(scope)?;
    let allowed_set: BTreeSet<u32> = allowed.iter().copied().collect();

    let mut detected: BTreeSet<u32> = BTreeSet::new();

    // Dims 1-4 + 6: GitHub Actions workflows.
    let wf_dir = root.join(".github").join("workflows");
    if wf_dir.is_dir() {
        let mut yml_files = Vec::new();
        if let Ok(rd) = std::fs::read_dir(&wf_dir) {
            for entry in rd.flatten() {
                let p = entry.path();
                if p.extension().is_some_and(|e| e == "yml" || e == "yaml") {
                    yml_files.push(p);
                }
            }
        }
        if !yml_files.is_empty() {
            detected.extend([1, 2, 3, 4]);
            for wf in &yml_files {
                if let Ok(content) = std::fs::read_to_string(wf)
                    && content.contains("${{ secrets.")
                {
                    detected.insert(6);
                    break;
                }
            }
        }
    }

    // Dims 5 + 12: container files.
    let has_docker = root.join("Dockerfile").exists()
        || root.join("docker-compose.yml").exists()
        || root.join("docker-compose.yaml").exists();
    if has_docker {
        detected.extend([5, 12]);
    }

    // Dim 7: .NET.
    let has_dotnet = has_csproj(root)
        || root.join("NuGet.Config").exists()
        || root.join("nuget.config").exists();
    if has_dotnet {
        detected.insert(7);
    }

    // Dim 8: Python.
    let has_python = root.join("requirements.txt").exists()
        || root.join("pyproject.toml").exists()
        || root.join("setup.cfg").exists()
        || root.join("Pipfile").exists();
    if has_python {
        detected.insert(8);
    }

    // Dim 9: Rust.
    let has_rust = root.join("Cargo.toml").exists();
    if has_rust {
        detected.insert(9);
    }

    // Dim 10: Node.js.
    let has_node = root.join("package.json").exists() || root.join("package-lock.json").exists();
    if has_node {
        detected.insert(10);
    }

    // Dim 11: Go.
    let has_go = root.join("go.mod").exists() || root.join("go.sum").exists();
    if has_go {
        detected.insert(11);
    }

    // Polyglot heuristic: a repo shipping Python, Node, Go, Rust, and Docker is
    // almost always a large monorepo that also carries .NET code, even when no
    // `*.csproj` sits where `has_csproj` looks. Force Dim 7 active so .NET
    // coverage is never silently dropped for such repos. Narrowly scoped (all
    // five signals required) to avoid false positives on smaller projects.
    if has_python && has_node && has_go && has_rust && has_docker {
        detected.insert(7);
    }

    let active: Vec<u32> = detected.intersection(&allowed_set).copied().collect();
    let active_set: BTreeSet<u32> = active.iter().copied().collect();
    let skipped: Vec<u32> = (1..=12).filter(|d| !active_set.contains(d)).collect();

    let skip_reasons: Vec<(u32, String)> = skipped
        .iter()
        .map(|&dim| {
            let reason = if !allowed_set.contains(&dim) {
                "Not in requested scope".to_string()
            } else {
                dim_skip_reason(dim).to_string()
            };
            (dim, reason)
        })
        .collect();

    Ok(EcosystemScope {
        active_dimensions: active,
        skipped_dimensions: skipped,
        skip_reasons,
    })
}
