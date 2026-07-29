//! Bounded, backend-neutral, fully explainable hybrid fusion contract
//! (issue #381).
//!
//! This pure walking skeleton declares the fusion strategy, candidate
//! lifecycle, score normalization, completeness, diversity integration, and
//! explainability boundaries for an ensemble of dense (DiskANN3), lexical
//! (Tantivy BM25), exact/reference, metadata, graph, and exhaustive
//! retrievers. It contains no live retrieval, scoring, backend binding, Store,
//! SQL, filesystem, daemon, CLI, or UI wiring.
//!
//! See `docs/architecture/hybrid-fusion.md`.

mod budget;
mod completeness;
mod error;
mod explainability;
mod kind;
mod model;
mod output;
mod profile;
mod stage;
mod workflow;

pub use budget::{
    FusionBudget, FusionBudgetDimension, FusionBudgetExhaustion, FusionBudgetFields, FusionUsage,
};
pub use completeness::{CompletenessState, CoverageAccount, ExhaustiveScopeId};
pub use error::{
    FusionDiagnosticCode, FusionError, FusionResult, HybridFusionDiagnosticCode, HybridFusionError,
    HybridFusionResult,
};
pub use explainability::{
    AppliedWeight, ExplainabilityReport, ExplainabilityReportFields, ExplainabilityRow,
    NormalizedScore,
};
pub use kind::RetrieverKind;
pub use model::{
    FilterIdentity, FusionCandidate, FusionCandidateFields, InclusionReason, ProvenanceEntry,
    RawRank, RawScore, RetrieverCandidate, RetrieverGeneration, RetrieverResult, ScoreDirection,
};
pub use output::{
    decode_fusion_stage_output_json, encode_fusion_stage_output_json, FusionStageOutput,
};
pub use profile::{
    ExplainabilityLevel, FusionProfile, FusionProfileFields, FusionStrategy, RetrieverWeight,
    ScoreNormalizationKind,
};
pub use stage::{FusionRun, FusionStage};
pub use workflow::{
    ContextPack, Exhaustive, ExploratoryMode, ExploratorySearch, FusionMode, FusionRequest,
    FusionRunResult, HybridFusionWorkflow, PrecisionRetrieve,
};

/// Contract schema version for hybrid-fusion documents.
pub const HYBRID_FUSION_CONTRACT_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
#[path = "../hybrid_fusion_tests.rs"]
mod tests;
