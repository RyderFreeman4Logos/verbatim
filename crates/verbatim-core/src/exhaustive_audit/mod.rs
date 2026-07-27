//! Completeness-aware exhaustive-audit workflow contract (WORKFLOW-008).
//!
//! This walking skeleton declares a finite scope, records deterministic primary
//! enumeration and per-member coverage, and fails closed when an exhaustive
//! claim cannot be established. Dense ANN, graph, and top-k retrieval remain
//! supplementary recall signals. No live Store, SQL, filesystem, retriever,
//! model, daemon, or CLI adapter is included.
//!
//! See `docs/architecture/exhaustive-audit-workflow.md`.

mod budget;
mod coverage;
mod enumeration;
mod error;
mod run;
mod scope;
mod stage;
mod util;
mod workflow;

pub use budget::{ExhaustiveAuditBudget, ExhaustiveAuditBudgetFields, ExhaustiveAuditUsage};
pub use coverage::{
    establish_completeness, CompletenessStatus, CompletenessTarget, CoverageEntry,
    CoverageManifest, CoverageManifestFields, ScopeCoverageStatus,
};
pub use enumeration::{
    CandidateEnumeration, CandidateEnumerationFields, CandidateOccurrence, DeduplicatedCandidate,
    DeduplicatedCandidateFields, EnumerationMethod,
};
pub use error::{
    ExhaustiveAuditBudgetDimension, ExhaustiveAuditBudgetExhaustion, ExhaustiveAuditError,
    ExhaustiveAuditResult,
};
pub use run::{
    decode_audit_workflow_run_json, encode_audit_workflow_run_json, AuditStageRecord, AuditWarning,
    AuditWorkflowRun, AuditWorkflowRunFields, EXHAUSTIVE_AUDIT_WORKFLOW_SCHEMA_VERSION,
};
pub use scope::{
    AuditScopeMember, AuditScopeMemberFields, DeclaredAuditScope, DeclaredAuditScopeFields,
    ScopeFreshness, ScopeIndexCoverage,
};
pub use stage::AuditStage;
pub use util::content_hash_of;
pub use workflow::{advance_stage, report, AuditOutcome, AuditTransition, ExhaustiveAuditWorkflow};

pub const EXHAUSTIVE_AUDIT_CONTRACT_SCHEMA_VERSION: u32 = EXHAUSTIVE_AUDIT_WORKFLOW_SCHEMA_VERSION;

#[cfg(test)]
#[path = "../exhaustive_audit_tests.rs"]
mod tests;
