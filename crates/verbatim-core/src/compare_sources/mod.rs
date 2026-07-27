//! Bounded compare-sources / version-differences workflow contract (WORKFLOW-007 / issue #358).
//!
//! This walking skeleton compares exactly two authorized, available source
//! versions. It separates evidence quotations from interpretations, records
//! deterministic artifacts and costs, and fails closed on ACL, lifecycle,
//! evidence, and budget failures. It contains no retriever, model, daemon,
//! CLI, SSE, or streaming implementation.
//!
//! Layering: this module uses canonical JSON/hashing from [`crate::wire_schemas`]
//! and is intended for adapters built over public [`crate::sdk`] and
//! [`crate::pagination`] contracts. A downstream adapter may materialize a
//! [`crate::wire_schemas::ContextPackEnvelope`] for grounded-answer consumers.
//! See `docs/architecture/compare-sources-workflow.md`.

mod budget;
mod dimension;
mod error;
mod result;
mod run;
mod scope;
mod stage;
mod util;
mod workflow;

pub use budget::{ComparisonBudget, ComparisonBudgetFields, ComparisonBudgetUsage};
pub use dimension::{
    ComparisonDimension, ComparisonDimensionFields, DimensionAlignment, DimensionValue,
    DimensionValueFields, EvidenceProvenance, QuotedEvidence,
};
pub use error::{
    ComparisonBudgetDimension, ComparisonBudgetExhaustion, ComparisonError, ComparisonResultType,
};
pub use result::{
    ComparisonCell, ComparisonContextPack, ComparisonContextPackFields, ComparisonResult,
    ComparisonResultFields,
};
pub use run::{
    content_hash_of, decode_workflow_run_json, encode_workflow_run_json, CompareSourcesWorkflowRun,
    CompareSourcesWorkflowRunFields, ComparisonCost, ComparisonRunStatus, ComparisonStageRecord,
    ComparisonWarning, ComparisonWarningSeverity, COMPARE_SOURCES_WORKFLOW_SCHEMA_VERSION,
};
pub use scope::{
    ComparisonScope, ComparisonScopeFields, SourceAvailability, SourceLifecycle, SourceVersion,
    SourceVersionFields,
};
pub use stage::ComparisonStage;
pub use workflow::{
    advance_stage, fail_closed, record_stage, start_run, try_complete, CompareSourcesWorkflow,
    ComparisonOutcome, ComparisonTransition, StageAdvance,
};

/// Contract schema version for compare-sources workflow documents.
pub const COMPARE_SOURCES_CONTRACT_SCHEMA_VERSION: u32 = COMPARE_SOURCES_WORKFLOW_SCHEMA_VERSION;

#[cfg(test)]
#[path = "../compare_sources_tests.rs"]
mod tests;
