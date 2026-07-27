//! Pure state-machine helpers and adapter trait for exhaustive audits.

use async_trait::async_trait;

use super::coverage::{establish_completeness, CompletenessStatus, CoverageManifest};
use super::enumeration::{CandidateEnumeration, DeduplicatedCandidate};
use super::error::{ExhaustiveAuditError, ExhaustiveAuditResult};
use super::run::AuditWorkflowRun;
use super::scope::DeclaredAuditScope;
use super::stage::AuditStage;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AuditTransition {
    pub from: AuditStage,
    pub to: AuditStage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AuditOutcome {
    Reported { status: CompletenessStatus },
    Disabled { detail: String },
}

impl AuditOutcome {
    pub fn status(&self) -> CompletenessStatus {
        match self {
            Self::Reported { status } => *status,
            Self::Disabled { .. } => CompletenessStatus::UnableToEstablish,
        }
    }
}

pub fn advance_stage(
    run: &mut AuditWorkflowRun,
    to: AuditStage,
) -> ExhaustiveAuditResult<AuditTransition> {
    if run.current_stage.is_terminal() || !is_legal_advance(run.current_stage, to) {
        return Err(ExhaustiveAuditError::IllegalTransition {
            detail: format!("cannot advance {:?} to {:?}", run.current_stage, to),
        });
    }
    let transition = AuditTransition {
        from: run.current_stage,
        to,
    };
    run.current_stage = to;
    Ok(transition)
}

fn is_legal_advance(from: AuditStage, to: AuditStage) -> bool {
    matches!(
        (from, to),
        (AuditStage::Declared, AuditStage::Enumerating)
            | (AuditStage::Enumerating, AuditStage::Covering)
            | (AuditStage::Covering, AuditStage::Reconciling)
            | (AuditStage::Reconciling, AuditStage::Reporting)
    )
}

/// Bind a report to the declared scope and make the final stage match the only
/// completeness status that the recorded evidence can justify.
pub fn report(
    run: &mut AuditWorkflowRun,
    scope: &DeclaredAuditScope,
    manifest: &CoverageManifest,
    enumerations: &[CandidateEnumeration],
) -> ExhaustiveAuditResult<AuditOutcome> {
    if run.current_stage != AuditStage::Reporting {
        return Err(ExhaustiveAuditError::IllegalTransition {
            detail: "audit reporting requires reporting stage".into(),
        });
    }
    let status = establish_completeness(run.target, scope, manifest, enumerations)?;
    let scope_hash = super::content_hash_of(scope)?;
    if run.scope_hash != scope_hash {
        return Err(ExhaustiveAuditError::validation(
            "run scope hash must match declared audit scope",
        ));
    }
    run.coverage_manifest_hash = Some(super::content_hash_of(manifest)?);
    run.enumeration_hashes = enumerations
        .iter()
        .map(super::content_hash_of)
        .collect::<ExhaustiveAuditResult<Vec<_>>>()?;
    run.query_fingerprints = enumerations
        .iter()
        .map(|enumeration| enumeration.query_fingerprint.clone())
        .collect();
    run.report_hash = Some(super::content_hash_of(&(scope, manifest, enumerations))?);
    run.status = status;
    run.current_stage = match status {
        CompletenessStatus::ExhaustiveOverDeclaredScope => AuditStage::Complete,
        CompletenessStatus::Incomplete => AuditStage::Incomplete,
        CompletenessStatus::UnableToEstablish => AuditStage::UnableToEstablish,
        CompletenessStatus::Blocked => AuditStage::Blocked,
    };
    Ok(AuditOutcome::Reported { status })
}

/// Adapter surface only. Implementations may call public SDK, pagination, and
/// wire contracts, but never upgrade a supplementary retrieval pass into an
/// exhaustive claim.
#[async_trait]
pub trait ExhaustiveAuditWorkflow: Send + Sync {
    async fn declare_scope(&self) -> ExhaustiveAuditResult<DeclaredAuditScope>;

    async fn enumerate(
        &self,
        run: &mut AuditWorkflowRun,
        scope: &DeclaredAuditScope,
    ) -> ExhaustiveAuditResult<Vec<CandidateEnumeration>>;

    async fn cover(
        &self,
        run: &mut AuditWorkflowRun,
        scope: &DeclaredAuditScope,
        enumerations: &[CandidateEnumeration],
    ) -> ExhaustiveAuditResult<CoverageManifest>;

    async fn reconcile(
        &self,
        run: &mut AuditWorkflowRun,
        enumerations: Vec<CandidateEnumeration>,
    ) -> ExhaustiveAuditResult<Vec<DeduplicatedCandidate>>;

    async fn report(
        &self,
        run: &mut AuditWorkflowRun,
        scope: &DeclaredAuditScope,
        manifest: CoverageManifest,
        enumerations: Vec<CandidateEnumeration>,
    ) -> ExhaustiveAuditResult<AuditOutcome>;
}
