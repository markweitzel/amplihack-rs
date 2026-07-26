//! amplihack-memory: Five-type cognitive memory system.
//!
//! Provides memory coordination, distributed sharding, bloom filter dedup,
//! and session discovery — matching the Python amplihack memory subsystem.

pub mod agent_memory;
pub mod auto_backend;
/// Lazy memory library availability check.
pub mod auto_install;
pub mod backend;
pub mod bloom;
pub mod config;
pub mod context_preservation;
pub mod coordinator;
pub mod database;
pub(crate) mod database_helpers;
pub mod discoveries;
pub mod distributed_store;
pub mod evaluation;
pub mod facade;
pub mod graph_db;
pub mod graph_store;
pub mod hash_ring;
pub mod maintenance;
pub mod manager;
pub mod memory_store;
pub mod models;
pub mod network_store;
pub(crate) mod network_store_types;
pub mod quality;
pub mod retrieval;
pub mod retrieval_pipeline;
pub mod sqlite_backend;
pub mod storage_pipeline;

/// Relocated code-graph / SCIP indexing command closure (issue #875).
///
/// Moved verbatim from `amplihack-cli`'s `commands::memory` module so that
/// lower-level crates (notably `amplihack-hooks`) can depend on these helpers
/// without pulling in the entire CLI. `amplihack-cli` re-exports this module as
/// `crate::commands::memory` to preserve its public API.
pub mod code_index;

/// Stable facade mirroring the former `amplihack_cli::memory` surface (issue
/// #875). These are the exact symbols `amplihack-hooks` consumes.
pub mod memory {
    pub use crate::code_index::{
        CodeGraphSummary, IndexStatus, PromptContextMemory, SessionSummary,
        background_index_job_active, background_index_job_path, check_index_status,
        default_code_graph_db_path_for_project, record_background_index_pid,
        resolve_code_graph_db_path_for_project, retrieve_prompt_context_memories,
        store_session_learning, summarize_code_graph,
    };
}

/// Hidden integration-test-only Kuzu FFI exports (issue #875). Mirrors the
/// former `amplihack_cli::memory::ffi_test_support` surface; `amplihack-cli`
/// re-exports these for its Kuzu FFI integration tests.
#[doc(hidden)]
pub mod code_graph_ffi_test_support {
    pub use crate::code_index::backend::graph_db::{
        graph_rows, init_graph_backend_schema, list_graph_sessions_from_conn,
    };
}

#[cfg(feature = "pyo3-bindings")]
pub mod pyo3_bindings;

pub use auto_backend::{DetectedBackend, detect_backend};
pub use backend::{BackendHealth, InMemoryBackend, MemoryBackend};
pub use bloom::BloomFilter;
pub use config::{Backend, MemoryConfig, Topology, Transport};
pub use coordinator::MemoryCoordinator;
#[cfg(feature = "sqlite")]
pub use database::MemoryDatabase;
pub use discoveries::{Discovery, get_recent_discoveries, store_discovery};
pub use distributed_store::DistributedGraphStore;
pub use evaluation::{
    BackendComparison, BackendReliabilityEvaluator, BackendReliabilityMetrics, BenchmarkEvaluator,
    BenchmarkMetrics, ComparisonReport, PerformanceContracts, QualityEvaluator, QualityMetrics,
    QualityReport, QueryTestCase, RetrievalQualityEvaluator, RetrievalQualityMetrics,
};
pub use facade::MemoryFacade;
pub use graph_store::GraphStore;
pub use hash_ring::HashRing;
#[cfg(feature = "sqlite")]
pub use maintenance::MemoryMaintenance;
pub use manager::MemoryManager;
pub use memory_store::InMemoryGraphStore;
pub use models::{MemoryEntry, MemoryQuery, MemoryType, SessionInfo, StorageRequest};
pub use network_store::{AgentRegistry, NetworkGraphStore};
pub use retrieval::{Fact, IntentKind, MemorySearch as RetrievalMemorySearch};
pub use retrieval_pipeline::{RetrievalPipeline, RetrievalResult, ScoredEntry};
pub use storage_pipeline::{StoragePipeline, StorageResult};
