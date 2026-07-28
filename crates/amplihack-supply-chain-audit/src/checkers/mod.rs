//! Per-dimension checkers. Each `check_*` returns the findings it detects.
//!
//! Scaffold surface for TDD — bodies are `unimplemented!()`.

use crate::schema::Finding;
use std::path::Path;

/// Dimension 1 — GitHub Actions SHA pinning.
pub fn check_action_sha_pinning(_root: &Path) -> Vec<Finding> {
    unimplemented!("check_action_sha_pinning is not yet implemented")
}

/// Dimension 2 — workflow permissions.
pub fn check_workflow_permissions(_root: &Path) -> Vec<Finding> {
    unimplemented!("check_workflow_permissions is not yet implemented")
}

/// Dimension 3 — secret exposure in workflow steps.
pub fn check_secret_exposure(_root: &Path) -> Vec<Finding> {
    unimplemented!("check_secret_exposure is not yet implemented")
}

/// Dimension 4 — cache poisoning.
pub fn check_cache_poisoning(_root: &Path) -> Vec<Finding> {
    unimplemented!("check_cache_poisoning is not yet implemented")
}

/// Dimension 5 — container base-image pinning.
pub fn check_container_image_pinning(_root: &Path) -> Vec<Finding> {
    unimplemented!("check_container_image_pinning is not yet implemented")
}

/// Dimension 6 — credential hygiene (OIDC vs. static secrets).
pub fn check_credential_hygiene(_root: &Path) -> Vec<Finding> {
    unimplemented!("check_credential_hygiene is not yet implemented")
}

/// Dimension 7 — NuGet lock / package source mapping.
pub fn check_nuget_lock(_root: &Path) -> Vec<Finding> {
    unimplemented!("check_nuget_lock is not yet implemented")
}

/// Dimension 8 — Python dependency integrity.
pub fn check_python_integrity(_root: &Path) -> Vec<Finding> {
    unimplemented!("check_python_integrity is not yet implemented")
}

/// Dimension 9 — Cargo supply chain.
pub fn check_cargo_supply_chain(_root: &Path) -> Vec<Finding> {
    unimplemented!("check_cargo_supply_chain is not yet implemented")
}

/// Dimension 10 — Node.js integrity.
pub fn check_node_integrity(_root: &Path) -> Vec<Finding> {
    unimplemented!("check_node_integrity is not yet implemented")
}

/// Dimension 11 — Go module integrity.
pub fn check_go_module_integrity(_root: &Path) -> Vec<Finding> {
    unimplemented!("check_go_module_integrity is not yet implemented")
}

/// Dimension 12 — Docker build chain (non-root, multi-stage hygiene).
pub fn check_docker_build_chain(_root: &Path) -> Vec<Finding> {
    unimplemented!("check_docker_build_chain is not yet implemented")
}
