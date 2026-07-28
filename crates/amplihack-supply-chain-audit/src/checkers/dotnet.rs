//! Dimension 7: NuGet / .NET dependency integrity checks.

use super::utils::{Counters, build, mk, relative_path};
use crate::schema::{Finding, Severity};
use once_cell::sync::Lazy;
use regex::Regex;
use std::path::{Path, PathBuf};
use walkdir::WalkDir;

static ADD_KEY: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?i)<add\s+key=").unwrap());

fn find_csproj(root: &Path) -> Vec<PathBuf> {
    let mut files = Vec::new();
    for entry in WalkDir::new(root).into_iter().flatten() {
        if entry
            .path()
            .extension()
            .is_some_and(|e| e.eq_ignore_ascii_case("csproj"))
        {
            files.push(entry.path().to_path_buf());
        }
    }
    files.sort();
    files
}

/// Dim 7: check NuGet lock files, locked-mode, and package source mapping.
pub fn check_nuget_lock(root: &Path) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut counters = Counters::new();

    for csproj in find_csproj(root) {
        let rel = relative_path(root, &csproj);
        let proj_dir = csproj.parent().unwrap_or(root);
        let has_lock = proj_dir.join("packages.lock.json").exists()
            || root.join("packages.lock.json").exists();
        let name = csproj
            .file_name()
            .map_or_else(String::new, |n| n.to_string_lossy().into_owned());

        if !has_lock {
            findings.push(build(mk(
                &mut counters,
                Severity::High,
                7,
                &rel,
                0,
                format!("No packages.lock.json found for {name}"),
                "Add to .csproj: <RestoreLockedMode>true</RestoreLockedMode>\n\
                 Run: dotnet restore --locked-mode to generate packages.lock.json",
                "Without a lock file, NuGet resolves to the latest compatible version \
                 on each restore, enabling silent dependency substitution attacks.",
            )));
        }

        if let Ok(content) = std::fs::read_to_string(&csproj)
            && !content.contains("RestoreLockedMode")
        {
            findings.push(build(mk(
                &mut counters,
                Severity::Info,
                7,
                &rel,
                0,
                "RestoreLockedMode not set",
                "<RestoreLockedMode>true</RestoreLockedMode>",
                "Add RestoreLockedMode=true to enforce lock file usage in CI. \
                     Prevents accidental resolution of newer versions.",
            )));
        }
    }

    for cfg_name in ["NuGet.Config", "nuget.config", "NuGet.config"] {
        let nuget_config = root.join(cfg_name);
        if !nuget_config.exists() {
            continue;
        }
        let rel = relative_path(root, &nuget_config);
        let Ok(content) = std::fs::read_to_string(&nuget_config) else {
            continue;
        };
        let lower = content.to_lowercase();
        let has_clear = lower.contains("<clear");
        let has_mapping = lower.contains("packagesourcemapping");
        let source_count = ADD_KEY.find_iter(&content).count();
        let multiple = source_count > 1;

        if multiple && !has_mapping {
            findings.push(build(mk(
                &mut counters,
                Severity::High,
                7,
                &rel,
                1,
                format!("NuGet.Config has {source_count} sources without packageSourceMapping"),
                "Add <packageSourceMapping> to map packages to specific sources:\n  \
                 <packageSourceMapping>\n    <packageSource key='internal'><package pattern='*' />\
                 </packageSource>\n  </packageSourceMapping>",
                "Multiple NuGet sources without packageSourceMapping enables \
                 dependency confusion attacks — an attacker can publish a higher-versioned \
                 package on nuget.org to override internal packages.",
            )));
        }

        if !has_clear && multiple {
            findings.push(build(mk(
                &mut counters,
                Severity::Medium,
                7,
                &rel,
                1,
                "NuGet.Config missing <clear /> before source list",
                "<packageSources><clear /><add key='internal' value='...' /></packageSources>",
                "Without <clear />, machine-level NuGet sources (including nuget.org) \
                 are inherited and may resolve packages from unintended sources.",
            )));
        }
        break;
    }

    findings
}
