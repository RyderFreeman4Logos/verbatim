//! Pure state-machine helpers and adapter trait for exhaustive audits.

use async_trait::async_trait;

use super::coverage::{establish_completeness, CompletenessStatus, CoverageManifest};
use super::enumeration::{CandidateEnumeration, DeduplicatedCandidate};
use super::error::{ExhaustiveAuditError, ExhaustiveAuditResult};
use super::run::{AuditWorkflowRun, ExhaustiveAuditEvidence};
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
    let coverage_manifest_hash = super::content_hash_of(manifest)?;
    let enumeration_hashes = enumerations
        .iter()
        .map(super::content_hash_of)
        .collect::<ExhaustiveAuditResult<Vec<_>>>()?;
    let primary_evidence = enumerations
        .iter()
        .zip(&enumeration_hashes)
        .find(|(enumeration, _)| enumeration.is_deterministic_primary())
        .map(|(enumeration, hash)| (hash.clone(), enumeration.query_fingerprint.clone()));
    let reconciliation_hash = super::content_hash_of(&(scope, manifest, enumerations))?;
    let report_hash = super::content_hash_of(&(run.target, scope, manifest, enumerations))?;
    run.coverage_manifest_hash = Some(coverage_manifest_hash.clone());
    run.enumeration_hashes = enumeration_hashes;
    run.primary_enumeration_hash = primary_evidence.as_ref().map(|(hash, _)| hash.clone());
    run.primary_query_fingerprint = primary_evidence
        .as_ref()
        .map(|(_, fingerprint)| fingerprint.clone());
    run.query_fingerprints = enumerations
        .iter()
        .map(|enumeration| enumeration.query_fingerprint.clone())
        .collect();
    run.reconciliation_hash = Some(reconciliation_hash.clone());
    run.report_hash = Some(report_hash.clone());
    run.status = status;
    run.exhaustive_evidence =
        (status == CompletenessStatus::ExhaustiveOverDeclaredScope).then(|| {
            ExhaustiveAuditEvidence {
                scope: scope.clone(),
                coverage_manifest: manifest.clone(),
                enumerations: enumerations.to_vec(),
            }
        });
    if status == CompletenessStatus::ExhaustiveOverDeclaredScope {
        let (primary_enumeration_hash, _) = primary_evidence.ok_or_else(|| {
            ExhaustiveAuditError::validation(
                "exhaustive status requires deterministic primary enumeration evidence",
            )
        })?;
        run.stage_records = vec![
            super::AuditStageRecord {
                stage: AuditStage::Declared,
                artifact_hash: scope_hash,
            },
            super::AuditStageRecord {
                stage: AuditStage::Enumerating,
                artifact_hash: primary_enumeration_hash,
            },
            super::AuditStageRecord {
                stage: AuditStage::Covering,
                artifact_hash: coverage_manifest_hash,
            },
            super::AuditStageRecord {
                stage: AuditStage::Reconciling,
                artifact_hash: reconciliation_hash,
            },
            super::AuditStageRecord {
                stage: AuditStage::Reporting,
                artifact_hash: report_hash,
            },
        ];
    } else {
        run.stage_records.clear();
    }
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
