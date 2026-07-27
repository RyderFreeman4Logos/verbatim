//! GroundedAnswerWorkflow trait and pure state-machine transitions.

use async_trait::async_trait;
use serde::{Deserialize, Serialize};

use super::answer::{AnswerDraft, AnswerPlan, GroundedAnswer};
use super::citation::{render_citations, CitationRenderRequest, RenderedCitationSet};
use super::claim::ClaimVerificationReport;
use super::error::{WorkflowError, WorkflowResult};
use super::policy::{PolicyDecision, WorkflowPolicyContext};
use super::run::WorkflowRun;
use super::stage::WorkflowStage;
use crate::wire_schemas::{
    ContextPackEnvelope, EvidencePackEnvelope, QueryPlanEnvelope, WorkflowEnvelope,
};

/// Outcome of a completed workflow attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum WorkflowOutcome {
    /// Published only verified supported claims.
    Published {
        answer: GroundedAnswer,
        run: WorkflowRun,
    },
    /// Typed abstention (never an unconstrained answer).
    Abstained { reason: String, run: WorkflowRun },
    /// Workflow disabled; caller should fall back to R/RA.
    Disabled { detail: String, run: WorkflowRun },
}

impl WorkflowOutcome {
    pub fn is_published(&self) -> bool {
        matches!(self, Self::Published { .. })
    }

    pub fn is_abstained(&self) -> bool {
        matches!(self, Self::Abstained { .. })
    }

    pub fn run(&self) -> &WorkflowRun {
        match self {
            Self::Published { run, .. }
            | Self::Abstained { run, .. }
            | Self::Disabled { run, .. } => run,
        }
    }
}

/// Explicit stage transition requested by an adapter / orchestrator.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowTransition {
    StartRetrieve,
    StartAssemble,
    StartGenerate,
    StartVerify,
    StartRender,
    Publish,
    Abstain,
    /// Single bounded revise: Verifying → Generating (once).
    ReviseOnce,
}

impl WorkflowTransition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::StartRetrieve => "start_retrieve",
            Self::StartAssemble => "start_assemble",
            Self::StartGenerate => "start_generate",
            Self::StartVerify => "start_verify",
            Self::StartRender => "start_render",
            Self::Publish => "publish",
            Self::Abstain => "abstain",
            Self::ReviseOnce => "revise_once",
        }
    }
}

/// Result of applying a transition.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageAdvance {
    pub from: WorkflowStage,
    pub to: WorkflowStage,
    pub transition: WorkflowTransition,
}

/// Pure state machine for legal stage advances.
///
/// Fail-closed: illegal transitions return [`WorkflowError::IllegalTransition`].
/// Terminal stages accept only no-ops (error on further advance).
pub fn advance_stage(
    current: WorkflowStage,
    transition: WorkflowTransition,
) -> WorkflowResult<StageAdvance> {
    if current.is_terminal() {
        return Err(WorkflowError::illegal_transition(
            current,
            current,
            "terminal stage cannot advance",
        ));
    }

    let to = match (current, transition) {
        (WorkflowStage::Planned, WorkflowTransition::StartRetrieve) => WorkflowStage::Retrieving,
        (WorkflowStage::Planned, WorkflowTransition::Abstain) => WorkflowStage::Abstained,
        (WorkflowStage::Retrieving, WorkflowTransition::StartAssemble) => WorkflowStage::Assembling,
        (WorkflowStage::Retrieving, WorkflowTransition::Abstain) => WorkflowStage::Abstained,
        (WorkflowStage::Assembling, WorkflowTransition::StartGenerate) => WorkflowStage::Generating,
        (WorkflowStage::Assembling, WorkflowTransition::Abstain) => WorkflowStage::Abstained,
        (WorkflowStage::Generating, WorkflowTransition::StartVerify) => WorkflowStage::Verifying,
        (WorkflowStage::Generating, WorkflowTransition::Abstain) => WorkflowStage::Abstained,
        (WorkflowStage::Verifying, WorkflowTransition::StartRender) => WorkflowStage::Rendering,
        (WorkflowStage::Verifying, WorkflowTransition::ReviseOnce) => WorkflowStage::Generating,
        (WorkflowStage::Verifying, WorkflowTransition::Abstain) => WorkflowStage::Abstained,
        (WorkflowStage::Rendering, WorkflowTransition::Publish) => WorkflowStage::Published,
        (WorkflowStage::Rendering, WorkflowTransition::Abstain) => WorkflowStage::Abstained,
        (from, _) => {
            return Err(WorkflowError::illegal_transition(
                from,
                from,
                format!("transition {} not allowed from {from}", transition.as_str()),
            ));
        }
    };

    Ok(StageAdvance {
        from: current,
        to,
        transition,
    })
}

/// Decide publish vs abstain from a verification report (fail-closed).
///
/// Only `all_publishable` reports may proceed to rendering/publish. Model
/// failures and partial/conflict verdicts force abstention (or revise when
/// allowed by the report + transition).
pub fn decide_after_verification(
    report: &ClaimVerificationReport,
) -> WorkflowResult<WorkflowTransition> {
    report.validate()?;
    if report.all_publishable {
        Ok(WorkflowTransition::StartRender)
    } else if report.revise_allowed {
        Ok(WorkflowTransition::ReviseOnce)
    } else {
        Ok(WorkflowTransition::Abstain)
    }
}

/// Build a published outcome only when answer validates; otherwise error
/// (caller must abstain — never invent a verified answer).
pub fn try_publish(
    mut run: WorkflowRun,
    answer: GroundedAnswer,
) -> WorkflowResult<WorkflowOutcome> {
    answer.validate()?;
    if run.current_stage != WorkflowStage::Rendering
        && run.current_stage != WorkflowStage::Published
    {
        return Err(WorkflowError::illegal_transition(
            run.current_stage,
            WorkflowStage::Published,
            "publish requires rendering stage",
        ));
    }
    run.publish(&answer)?;
    Ok(WorkflowOutcome::Published { answer, run })
}

/// Fail closed to typed abstention; never returns a fabricated answer.
pub fn abstain_outcome(
    mut run: WorkflowRun,
    reason: impl Into<String>,
) -> WorkflowResult<WorkflowOutcome> {
    let reason = reason.into();
    run.abstain(reason.clone())?;
    Ok(WorkflowOutcome::Abstained { reason, run })
}

/// Map any model/verify failure into abstention (or disabled when appropriate).
pub fn fail_closed(run: WorkflowRun, err: WorkflowError) -> WorkflowResult<WorkflowOutcome> {
    match err {
        WorkflowError::Disabled { detail } => {
            let detail = detail.unwrap_or_else(|| "workflow disabled".into());
            let mut run = run;
            run.mark_disabled(detail.clone())?;
            Ok(WorkflowOutcome::Disabled { detail, run })
        }
        other => {
            let reason = other.to_string();
            abstain_outcome(run, reason)
        }
    }
}

/// Trait surface for the bounded grounded-answer workflow.
///
/// Implementations may call models and storage **only** through public SDK
/// types. This walking skeleton provides the pure contracts; live adapters are
/// residual.
#[async_trait]
pub trait GroundedAnswerWorkflow: Send + Sync {
    /// Create a new run envelope for a QueryPlan (no model call).
    fn begin_run(
        &self,
        query_plan: &QueryPlanEnvelope,
        run_id: String,
        policy: &WorkflowPolicyContext,
    ) -> WorkflowResult<WorkflowRun>;

    /// Evaluate policy gates for the next stage (implementation residual).
    fn evaluate_policy(
        &self,
        policy: &WorkflowPolicyContext,
        stage: WorkflowStage,
    ) -> WorkflowResult<Vec<PolicyDecision>>;

    /// Retrieve → EvidencePack (adapter residual; trait contract only).
    async fn retrieve(
        &self,
        run: &mut WorkflowRun,
        query_plan: &QueryPlanEnvelope,
    ) -> WorkflowResult<EvidencePackEnvelope>;

    /// Assemble → ContextPack.
    async fn assemble(
        &self,
        run: &mut WorkflowRun,
        evidence: &EvidencePackEnvelope,
    ) -> WorkflowResult<ContextPackEnvelope>;

    /// Plan + draft generate (model optional path).
    async fn generate_draft(
        &self,
        run: &mut WorkflowRun,
        context: &ContextPackEnvelope,
        plan: &AnswerPlan,
    ) -> WorkflowResult<AnswerDraft>;

    /// Claim-level verification against context.
    async fn verify_claims(
        &self,
        run: &mut WorkflowRun,
        context: &ContextPackEnvelope,
        draft: &AnswerDraft,
    ) -> WorkflowResult<ClaimVerificationReport>;

    /// Deterministic citation rendering for publishable claims.
    fn render_citations(
        &self,
        request: &CitationRenderRequest,
    ) -> WorkflowResult<RenderedCitationSet> {
        render_citations(request)
    }

    /// Full pipeline entry (adapters implement; pure fail-closed helpers above
    /// are available for composition).
    async fn run(
        &self,
        query_plan: &QueryPlanEnvelope,
        policy: &WorkflowPolicyContext,
        run_id: String,
    ) -> WorkflowResult<WorkflowOutcome>;
}

/// Project a [`WorkflowRun`] into the thinner wire [`WorkflowEnvelope`].
pub fn workflow_run_to_envelope(
    run: &WorkflowRun,
    artifact_id: impl Into<String>,
) -> WorkflowResult<WorkflowEnvelope> {
    use crate::wire_schemas::{WorkflowEnvelopeFields, WorkflowPhase};

    let phase = match run.final_status {
        super::run::WorkflowFinalStatus::Published => WorkflowPhase::Completed,
        super::run::WorkflowFinalStatus::Abstained | super::run::WorkflowFinalStatus::Disabled => {
            WorkflowPhase::Failed
        }
        super::run::WorkflowFinalStatus::InProgress => match run.current_stage {
            WorkflowStage::Planned => WorkflowPhase::Planned,
            WorkflowStage::Retrieving => WorkflowPhase::Retrieving,
            WorkflowStage::Assembling => WorkflowPhase::Assembling,
            WorkflowStage::Generating => WorkflowPhase::Generating,
            WorkflowStage::Verifying | WorkflowStage::Rendering => WorkflowPhase::Verifying,
            WorkflowStage::Published => WorkflowPhase::Completed,
            WorkflowStage::Abstained => WorkflowPhase::Failed,
        },
    };

    WorkflowEnvelope::new(WorkflowEnvelopeFields {
        artifact_id: artifact_id.into(),
        phase,
        query_plan_hash: run.query_plan_hash.clone(),
        evidence_pack_hash: run.evidence_pack_hash.clone(),
        context_pack_hash: run.context_pack_hash.clone(),
        generation: run.generation.clone(),
        profile_ref: run.profile_ref.clone(),
    })
    .map_err(|err| WorkflowError::validation(err.to_string()))
}
