//! Native Rust implementation of the `supply-chain-audit` skill.
//!
//! Audits software supply-chain security across CI/CD pipelines, container
//! images, and language package ecosystems, emitting structured findings with
//! severity ratings, `file:line` references, and copy-pasteable fixes.
//!
//! This crate replaces the upstream Python package `supply_chain_audit/` with
//! equivalent detection logic, the same finding schema, the same report format,
//! and the same security invariants.

#![forbid(unsafe_code)]
// TDD scaffold phase: module bodies are `unimplemented!()`, so struct fields
// and internal constructors are not yet read. This crate-level allow keeps the
// `-D warnings` clippy gate green while the failing tests define the contract;
// the implementation step removes it once every field/constructor is wired up.
#![allow(dead_code)]

pub mod audit;
pub mod checkers;
pub mod detector;
pub mod error;
pub mod external_tools;
pub mod report;
pub mod schema;

pub use audit::{AuditConfig, AuditResult, run_audit};
pub use checkers::{
    check_action_sha_pinning, check_cache_poisoning, check_cargo_supply_chain,
    check_container_image_pinning, check_credential_hygiene, check_docker_build_chain,
    check_go_module_integrity, check_node_integrity, check_nuget_lock, check_python_integrity,
    check_secret_exposure, check_workflow_permissions,
};
pub use detector::{EcosystemScope, detect_ecosystems};
pub use error::{Result, SupplyChainAuditError, VALID_SCOPES};
pub use report::{AuditReport, SlsaAssessment};
pub use schema::{Finding, FindingId, Severity, sanitize_for_display, validate_finding};
