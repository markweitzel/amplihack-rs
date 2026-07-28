//! Ecosystem detection — maps repo file signals to audit dimensions.

use crate::error::Result;
use std::path::Path;

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

    /// Human-readable reason a dimension was skipped.
    pub fn get_skip_reason(&self, dim: u32) -> String {
        self.skip_reasons
            .iter()
            .find(|(d, _)| *d == dim)
            .map(|(_, r)| r.clone())
            .unwrap_or_else(|| "No matching files found".to_string())
    }
}

/// Detect which dimensions are active based on files present in `root`.
///
/// Validates `scope` against the strict allowlist first (before any file
/// system access), returning [`crate::error::SupplyChainAuditError::InvalidScope`]
/// for unknown or injection-attempt values.
pub fn detect_ecosystems(_root: &Path, _scope: &str) -> Result<EcosystemScope> {
    unimplemented!("detect_ecosystems is not yet implemented")
}
