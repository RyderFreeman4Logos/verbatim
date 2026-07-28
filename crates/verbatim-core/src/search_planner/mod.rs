//! Hard-bounded SearchBudget and selectivity-aware retrieval planning contract.
//!
//! This pure walking skeleton declares authorization-safe cardinality estimation,
//! bounded path selection, generation binding, and public work reporting. It has no
//! live SQLite, Qdrant, Tantivy, DiskANN3, filesystem, daemon, or CLI wiring.
//!
//! See `docs/architecture/search-budget-planner.md`.

mod budget;
mod candidate_profile;
mod capability;
mod cardinality;
mod error;
mod plan;
mod planner;
mod report;
mod request;
mod selectivity;
mod usage;

pub use budget::{SearchBudget, SearchBudgetFields};
pub use candidate_profile::{
    CandidateQuantizer, FullPrecisionRescoreBudget, QuantizedCandidateGenerationProfile,
};
pub use capability::{
    BackendCapability, BackendCapabilityFields, ColdCacheBehavior, MemoryTierBehavior, VectorMetric,
};
pub use cardinality::{
    CardinalityConfidence, CardinalityEstimate, CardinalityFreshness, CardinalitySource,
    EstimateHandlingMode, EstimateReliability,
};
pub use error::{SearchPlannerDiagnosticCode, SearchPlannerError, SearchPlannerResult};
pub use plan::{
    GenerationBinding, NamedDegradedProfile, PlanIdentity, PlannedCompleteness, RetrievalPlan,
    SelectedPath,
};
pub use planner::{SearchPlanner, SearchPlannerContract};
pub use report::{CompletionState, PublicRetrievalRecord};
pub use request::{
    PlannerRequest, RetrievalIntent, StrictPredicateHandlingMode, WarmCacheAttestation,
};
pub use selectivity::{CrossoverThresholds, SelectivityClass, SelectivityProfile};
pub use usage::SearchBudgetUsage;

/// Contract schema version for SearchBudget and retrieval-planner documents.
pub const SEARCH_PLANNER_CONTRACT_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
#[path = "../search_planner_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "../search_planner_selection_tests.rs"]
mod selection_tests;
