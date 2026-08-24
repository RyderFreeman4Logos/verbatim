//! Bounded grounded-answer workflow contract (WORKFLOW-005 / issue #356).
//!
//! Walking skeleton: typed stage pipeline and artifacts for
//! QueryPlan → EvidencePack → ContextPack → AnswerPlan/draft → claim
//! verification → deterministic citation rendering → published
//! [`GroundedAnswer`] or typed abstention. Fail-closed: model/schema/policy
//! failure never becomes a verified answer. No live model calls, daemon/CLI
//! wiring, SSE, or filesystem access. The constrained selector is the sole
//! Store boundary: it validates persisted citable ids but never exposes text.
//!
//! Residual: live model integration, policy engine implementation, ADK-Rust
//! package wiring, benchmarks, streaming, closing #356.
//! See `docs/architecture/grounded-answer-workflow.md`.
//!
//! Layering: reuses [`crate::wire_schemas`] (API-002), [`crate::pagination`]
//! (API-003), and [`crate::sdk`] operation envelopes (SDK-001). Apart from
//! the selector's private Store validation, it does **not** import SQL or
//! filesystem types.

mod answer;
mod citation;
mod claim;
mod error;
mod policy;
mod run;
mod selector;
mod stage;
mod workflow;

pub use answer::{
    AnswerDraft, AnswerPlan, AnswerPlanFields, GroundedAnswer, GroundedAnswerFields, GroundedClaim,
    GroundedClaimFields,
};
pub use citation::{
    render_citations, CitationRenderRequest, CitationRenderRequestFields, CitationStyle,
    ClaimCitationBinding, RenderedCitation, RenderedCitationSet,
};
pub use claim::{
    ClaimId, ClaimSupportClass, ClaimVerdict, ClaimVerificationReport,
    ClaimVerificationReportFields, DraftClaim, DraftClaimFields, QuotationCheck,
    QuotationCheckStatus,
};
pub use error::{WorkflowError, WorkflowResult};
pub use policy::{
    PolicyDecision, PolicyDecisionKind, PolicyGate, PolicyGateKind, WorkflowPolicyContext,
    WorkflowPolicyContextFields,
};
pub use run::{
    content_hash_of, decode_workflow_run_json, encode_workflow_run_json, WorkflowCost,
    WorkflowFinalStatus, WorkflowRun, WorkflowRunFields, WorkflowRunRecord, WorkflowStageRecord,
    WorkflowWarning, WorkflowWarningSeverity, GROUNDED_ANSWER_WORKFLOW_SCHEMA_VERSION,
};
pub use selector::{select_persisted_evidence, EvidenceIdSelector, EvidenceSelectionResult};
pub use stage::WorkflowStage;
pub use workflow::{
    abstain_outcome, advance_stage, decide_after_verification, fail_closed, try_publish,
    workflow_run_to_envelope, GroundedAnswerWorkflow, StageAdvance, WorkflowOutcome,
    WorkflowTransition,
};

/// Contract schema version for grounded-answer workflow documents.
/// Unknown versions fail closed on decode.
pub const GROUNDED_ANSWER_CONTRACT_SCHEMA_VERSION: u32 = GROUNDED_ANSWER_WORKFLOW_SCHEMA_VERSION;

#[cfg(test)]
#[path = "../grounded_answer_tests.rs"]
mod tests;
