//! External tool availability layer (`gh`, `crane`, `syft`, `grype`, `cosign`).
//!
//! This module reports which optional tools are on `PATH`, their documented
//! per-tool timeouts, and install instructions for any that are missing. It
//! never spawns processes itself — the audit runs fully offline and only
//! surfaces tool availability so the report can flag degraded coverage.

use std::collections::BTreeMap;

/// The five optional external tools, in stable order.
pub const TOOL_NAMES: [&str; 5] = ["gh", "crane", "syft", "grype", "cosign"];

/// Documented per-tool timeout (seconds). Returns `None` for unknown tools.
pub fn tool_timeout(name: &str) -> Option<u64> {
    match name {
        "gh" => Some(15),
        "crane" => Some(20),
        "syft" => Some(120),
        "grype" => Some(60),
        "cosign" => Some(30),
        _ => None,
    }
}

/// Install metadata for a missing tool.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MissingTool {
    pub name: String,
    pub description: String,
    pub install_options: Vec<String>,
}

/// Availability status for every tool: `name -> status string`.
///
/// Status strings contain `"available"` or `"unavailable"`.
pub fn check_tool_availability() -> BTreeMap<String, String> {
    let mut map = BTreeMap::new();
    for name in TOOL_NAMES {
        let timeout = tool_timeout(name).unwrap_or(0);
        let status = if which::which(name).is_ok() {
            format!("available (timeout: {timeout}s)")
        } else {
            "unavailable (not found in PATH)".to_string()
        };
        map.insert(name.to_string(), status);
    }
    map
}

/// Missing tools with install instructions. Empty when all are present.
pub fn check_missing_tools() -> Vec<MissingTool> {
    TOOL_NAMES
        .iter()
        .filter(|name| which::which(name).is_err())
        .filter_map(|name| install_options(name))
        .collect()
}

/// Install metadata (description + options) for a tool, if known.
pub fn install_options(name: &str) -> Option<MissingTool> {
    let (description, options): (&str, &[&str]) = match name {
        "gh" => (
            "GitHub CLI — required for provenance/attestation checks",
            &[
                "linux_apt: sudo apt install -y gh",
                "linux_dnf: sudo dnf install -y gh",
                "macos: brew install gh",
            ],
        ),
        "crane" => (
            "go-containerregistry crane — required for container image inspection",
            &["go_install: go install github.com/google/go-containerregistry/cmd/crane@latest"],
        ),
        "syft" => (
            "Anchore syft — required for SBOM generation",
            &[
                "script: curl -sSfL https://raw.githubusercontent.com/anchore/syft/main/install.sh | sh -s -- -b /usr/local/bin",
            ],
        ),
        "grype" => (
            "Anchore grype — required for vulnerability scanning",
            &[
                "script: curl -sSfL https://raw.githubusercontent.com/anchore/grype/main/install.sh | sh -s -- -b /usr/local/bin",
            ],
        ),
        "cosign" => (
            "Sigstore cosign — required for signature verification",
            &["go_install: go install github.com/sigstore/cosign/v2/cmd/cosign@latest"],
        ),
        _ => return None,
    };
    Some(MissingTool {
        name: name.to_string(),
        description: description.to_string(),
        install_options: options.iter().map(|s| s.to_string()).collect(),
    })
}
