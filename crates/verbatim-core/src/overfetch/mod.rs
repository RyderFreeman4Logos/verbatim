//! Bounded retrieval-orchestration contract that eliminates normal-query overfetch.
//!
//! This pure walking skeleton declares count, candidate, backend-selection,
//! validation, hydration, instrumentation, and report boundaries. It contains no
//! live SQLite, Tantivy, Qdrant, DiskANN3, filesystem, daemon, or CLI wiring.
//! See `docs/architecture/overfetch-elimination.md`.

mod backend;
mod budget;
mod contract;
mod count;
mod error;
mod hydration;
mod instrumentation;
mod policy;

pub use backend::{
    PrimaryBackendOutcome, PrimaryBackendSelection, RetrievalBackend, TypedBackendFailure,
};
pub use budget::{RetrieverKind, SearchBudget, SearchBudgetFields};
pub use contract::{
    decode_retrieval_plan_json, decode_search_budget_json, encode_retrieval_plan_json,
    encode_search_budget_json, BoundedRetrievalContract, ComplexityInvariant, DebugOutput,
    DiagnosticMode, FusedCandidates, RetrievalPlan, RetrieverCandidates,
};
pub use count::CountPort;
pub use error::{OverfetchError, OverfetchResult};
pub use hydration::{BatchHydrationPort, FullHydration, HydrationBatch};
pub use instrumentation::{HydrationBatchKind, StatementCountInstrumentation};
pub use policy::{
    AdaptiveOverfetchPolicy, AdaptiveOverfetchPolicyFields, CandidateId, CandidateValidation,
    LifecycleState, RetrievalCandidate, RetrievalFilters, StrictFilter, StrictFilterSupport,
    ValidatedCandidates,
};

/// Contract schema version for bounded retrieval orchestration documents.
pub const OVERFETCH_CONTRACT_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
#[path = "../overfetch_tests.rs"]
mod tests;
