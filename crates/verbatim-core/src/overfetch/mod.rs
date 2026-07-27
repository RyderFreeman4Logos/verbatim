//! Bounded retrieval-orchestration contract that eliminates normal-query overfetch.
//!
//! This pure walking skeleton declares count, candidate, backend-selection,
//! validation, hydration, instrumentation, and report boundaries. It contains no
//! live SQLite, Tantivy, Qdrant, DiskANN3, filesystem, daemon, or CLI wiring.
//! See `docs/architecture/overfetch-elimination.md`.

mod backend;
mod budget;
#[cfg_attr(not(test), allow(dead_code))]
mod contract;
mod count;
mod error;
#[cfg_attr(not(test), allow(dead_code))]
mod hydration;
mod instrumentation;
#[cfg_attr(not(test), allow(dead_code))]
mod policy;

pub use backend::{
    PrimaryBackendOutcome, PrimaryBackendSelection, RetrievalBackend, TypedBackendFailure,
};
pub use budget::{RetrieverKind, SearchBudget, SearchBudgetFields};
pub use contract::{
    decode_search_budget_json, encode_search_budget_json, ComplexityInvariant, DebugOutput,
    DiagnosticMode,
};
pub use error::{OverfetchError, OverfetchResult};
pub use instrumentation::{HydrationBatchKind, StatementCountInstrumentation};
pub use policy::{
    AdaptiveOverfetchPolicy, AdaptiveOverfetchPolicyFields, CandidateId, LifecycleState,
    RetrievalCandidate, RetrievalFilters, StrictFilter, StrictFilterSupport,
};

/// Contract schema version for bounded retrieval orchestration documents.
pub const OVERFETCH_CONTRACT_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
#[path = "../overfetch_tests.rs"]
mod tests;
