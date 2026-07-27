//! State machine and adapter trait for citation-audit execution.

use async_trait::async_trait;

use super::{
    AuditDocument, AuditTextOrigin, CitationAuditError, CitationAuditResult, CitationAuditRun,
    CitationAuditRunStatus, CitationAuditStage, ClaimAuditResult, ClaimCoverageEnvelope,
    ClaimRecord, ClaimSegmentation, EvidenceCandidate, EvidenceRegistry,
};

/// Refuse an attempt to make document/evidence/model text a tool or workflow
/// control channel. Future live adapters must call this at their boundary.
pub fn guard_workflow_control(origin: AuditTextOrigin) -> CitationAuditResult<()> {
    if !origin.may_alter_workflow_control() {
        return Err(CitationAuditError::UntrustedControl {
            detail: "untrusted audit text cannot alter workflow control".into(),
        });
    }
    Ok(())
}

/// Advance the run through the only legal ordered transitions.
pub fn advance_stage(
    run: &mut CitationAuditRun,
    to: CitationAuditStage,
) -> CitationAuditResult<()> {
    let from = run.current_stage;
    let legal = matches!(
        (from, to),
        (
            CitationAuditStage::Segmenting,
            CitationAuditStage::Retrieving
        ) | (
            CitationAuditStage::Retrieving,
            CitationAuditStage::Classifying
        ) | (
            CitationAuditStage::Classifying,
            CitationAuditStage::Validating
        ) | (
            CitationAuditStage::Validating,
            CitationAuditStage::Aggregating
        ) | (
            CitationAuditStage::Aggregating,
            CitationAuditStage::Complete
        ) | (
            CitationAuditStage::Segmenting,
            CitationAuditStage::Incomplete
        ) | (
            CitationAuditStage::Retrieving,
            CitationAuditStage::Incomplete
        ) | (
            CitationAuditStage::Classifying,
            CitationAuditStage::Incomplete
        ) | (
            CitationAuditStage::Validating,
            CitationAuditStage::Incomplete
        ) | (
            CitationAuditStage::Aggregating,
            CitationAuditStage::Incomplete
        ) | (CitationAuditStage::Segmenting, CitationAuditStage::Disabled)
    );
    if !legal || run.status != CitationAuditRunStatus::Running {
        return Err(CitationAuditError::IllegalTransition { from, to });
    }
    run.current_stage = to;
    run.status = match to {
        CitationAuditStage::Complete => CitationAuditRunStatus::Complete,
        CitationAuditStage::Incomplete => CitationAuditRunStatus::Incomplete,
        CitationAuditStage::Disabled => CitationAuditRunStatus::Disabled,
        _ => CitationAuditRunStatus::Running,
    };
    run.validate()
}

/// Adapter surface only. This trait intentionally has no core implementation:
/// adapters may use retrieval/model services, but must resolve evidence IDs and
/// quotes server-side before any final aggregate is accepted.
#[async_trait]
pub trait CitationAuditWorkflow: Send + Sync {
    /// Decompose externally supplied prose into stable source-offset claims.
    async fn decompose(
        &self,
        run: &mut CitationAuditRun,
        document: &AuditDocument,
    ) -> CitationAuditResult<ClaimSegmentation>;

    /// Retrieve opaque candidates with recorded strategy; candidates are not
    /// evidence until deterministic validation later.
    async fn retrieve_candidates(
        &self,
        run: &mut CitationAuditRun,
        claim: &ClaimRecord,
    ) -> CitationAuditResult<Vec<EvidenceCandidate>>;

    /// Produce a constrained classification proposal for one claim.
    async fn classify(
        &self,
        run: &mut CitationAuditRun,
        claim: &ClaimRecord,
        candidates: &[EvidenceCandidate],
    ) -> CitationAuditResult<ClaimAuditResult>;

    /// Resolve every proposed ID and quote against server evidence; reject
    /// unknown IDs and altered quotations rather than repairing them.
    async fn validate(
        &self,
        run: &mut CitationAuditRun,
        segmentation: &ClaimSegmentation,
        results: Vec<ClaimAuditResult>,
        registry: &EvidenceRegistry,
    ) -> CitationAuditResult<Vec<ClaimAuditResult>>;

    /// Persist/return aggregate coverage only after validated per-claim results.
    async fn aggregate(
        &self,
        run: &mut CitationAuditRun,
        segmentation: &ClaimSegmentation,
        results: &[ClaimAuditResult],
        registry: &EvidenceRegistry,
    ) -> CitationAuditResult<ClaimCoverageEnvelope>;
}
