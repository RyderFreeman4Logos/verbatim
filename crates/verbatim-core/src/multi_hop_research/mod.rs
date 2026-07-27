//! Bounded multi-hop research workflow contract (WORKFLOW-006 / issue #357).
//!
//! Walking skeleton: typed decomposition → parallel subqueries → coverage
//! evaluation → bounded corrective rounds → merged ContextPack. Fail-closed
//! budgets, injection-resistant evidence origins, and incomplete status when
//! coverage is insufficient. No live model calls, Store access, SSE, or
//! daemon/CLI wiring.
//!
//! Residual: live retrieval adapters, graph edge evidence enforcement beyond
//! types, ADK-Rust package wiring, benchmarks, closing #357.
//! See `docs/architecture/multi-hop-research-workflow.md`.
//!
//! Layering: reuses [`crate::wire_schemas`] (API-002), [`crate::pagination`]
//! (API-003), and [`crate::sdk`] operation envelopes (SDK-001). Does **not**
//! import Store/SQL/filesystem types. Complements [`crate::grounded_answer`]
//! (WORKFLOW-005) which consumes a ContextPack for answer publication.

mod budget;
mod coverage;
mod decomposition;
mod error;
mod evidence;
mod merge;
mod run;
mod stage;
mod subquery;
mod util;
mod workflow;

pub use budget::{ResearchBudget, ResearchBudgetFields, ResearchBudgetUsage};
pub use coverage::{
    CoverageConflict, CoverageReport, CoverageReportFields, CoverageStatus, FactCoverage,
    RelationCoverage,
};
pub use decomposition::{
    DecompositionPlan, DecompositionPlanFields, ResearchQuestion, ResearchQuestionFields,
    RetrieverKind, SubQuestion, SubQuestionFields, SubQuestionId,
};
pub use error::{BudgetDimension, BudgetExhaustion, ResearchError, ResearchResult};
pub use evidence::{
    guard_instruction_origin, EvidenceOrigin, EvidenceOriginFields, EvidenceOriginKind,
};
pub use merge::{
    merge_attributed_units, AttributedEvidenceUnit, MergedContextPack, MergedContextPackFields,
};
pub use run::{
    content_hash_of, decode_workflow_run_json, encode_workflow_run_json, ResearchFinalStatus,
    ResearchRoundRecord, ResearchWarning, ResearchWarningSeverity, WorkflowRun, WorkflowRunFields,
    WorkflowRunRecord, MULTI_HOP_RESEARCH_WORKFLOW_SCHEMA_VERSION,
};
pub use stage::ResearchRound;
pub use subquery::{
    ParallelRetrievalBatch, ParallelRetrievalBatchFields, ParallelRetrievalBatchResult,
    RetrieverProvenance, SubqueryRequest, SubqueryRequestFields, SubqueryResult,
};
pub use workflow::{
    advance_round, decide_after_coverage, fail_closed, record_coverage, record_decomposition,
    try_complete, CoverageDecision, MultiHopResearchWorkflow, ResearchOutcome, ResearchTransition,
    RoundAdvance,
};

/// Contract schema version for multi-hop research workflow documents.
/// Unknown versions fail closed on decode.
pub const MULTI_HOP_RESEARCH_CONTRACT_SCHEMA_VERSION: u32 =
    MULTI_HOP_RESEARCH_WORKFLOW_SCHEMA_VERSION;

#[cfg(test)]
#[path = "../multi_hop_research_tests.rs"]
mod tests;
