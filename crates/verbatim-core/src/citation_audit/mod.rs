//! Citation-audit / claim-support workflow contract (WORKFLOW-009 / issue #360).
//!
//! This walking skeleton accepts externally authored prose, preserves optional
//! existing citation markup as untrusted input, and produces only
//! source-offset-bound claims plus evidence classifications validated against a
//! server-resolved evidence registry. It contains no retriever, model, Store,
//! SQL, filesystem, daemon, or CLI implementation.
//!
//! The workflow is usable without generation. Any future revision publication
//! must enter the separate [`crate::grounded_answer`] publication contract.
//! See `docs/architecture/citation-audit-workflow.md`.

mod budget;
mod error;
mod evidence;
mod input;
mod result;
mod run;
mod stage;
mod util;
mod workflow;

pub use budget::{
    CitationAuditBudget, CitationAuditBudgetDimension, CitationAuditBudgetExhaustion,
    CitationAuditBudgetFields, CitationAuditUsage,
};
pub use error::{CitationAuditError, CitationAuditResult};
pub use evidence::{
    EvidenceCandidate, EvidenceReference, EvidenceRegistry, ResolvedEvidence, RetrievalStrategy,
};
pub use input::{
    AuditDocument, AuditDocumentFields, ClaimId, ClaimRecord, ClaimRecordFields, ClaimSegmentation,
    UntrustedExistingCitation,
};
pub use result::{
    Calibration, CalibrationStatus, ClaimAuditResult, ClaimAuditResultFields, ClaimConflict,
    ClaimCoverageCounts, ClaimCoverageEnvelope, CoverageStatus, EvidenceClassification,
    SourceApplicability,
};
pub use run::{
    complete_run, decode_citation_audit_run_json, encode_citation_audit_run_json, CitationAuditRun,
    CitationAuditRunStatus, CITATION_AUDIT_WORKFLOW_SCHEMA_VERSION,
};
pub use stage::{AuditTextOrigin, CitationAuditStage};
pub use util::content_hash_of;
pub use workflow::{advance_stage, guard_workflow_control, CitationAuditWorkflow};

/// Contract schema version for citation-audit workflow documents.
pub const CITATION_AUDIT_CONTRACT_SCHEMA_VERSION: u32 = CITATION_AUDIT_WORKFLOW_SCHEMA_VERSION;

/// Create a run without enabling generation or wiring a live adapter.
pub fn start_run(
    run_id: String,
    document: &AuditDocument,
    budget: CitationAuditBudget,
) -> CitationAuditResult<CitationAuditRun> {
    CitationAuditRun::new(run_id, document, budget)
}

#[cfg(test)]
#[path = "../citation_audit_tests.rs"]
mod tests;
