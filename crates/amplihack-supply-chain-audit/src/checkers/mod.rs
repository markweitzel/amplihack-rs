//! Per-dimension checkers. Each `check_*` returns the findings it detects,
//! with per-checker sequential IDs (the audit orchestrator reassigns global
//! IDs afterwards).

mod actions;
mod containers;
mod credentials;
mod dotnet;
mod go;
mod node;
mod python;
mod rust;
pub(crate) mod utils;

pub use actions::{
    check_action_sha_pinning, check_cache_poisoning, check_secret_exposure,
    check_workflow_permissions,
};
pub use containers::{check_container_image_pinning, check_docker_build_chain};
pub use credentials::check_credential_hygiene;
pub use dotnet::check_nuget_lock;
pub use go::check_go_module_integrity;
pub use node::check_node_integrity;
pub use python::check_python_integrity;
pub use rust::check_cargo_supply_chain;
