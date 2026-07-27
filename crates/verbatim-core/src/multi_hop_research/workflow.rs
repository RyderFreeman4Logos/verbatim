//! Round state machine and MultiHopResearchWorkflow trait surface.

use async_trait::async_trait;

use super::budget::ResearchBudget;
use super::coverage::CoverageReport;
use super::decomposition::{DecompositionPlan, ResearchQuestion};
use super::error::{ResearchError, ResearchResult};
use super::merge::MergedContextPack;
use super::run::{
    content_hash_of, ResearchFinalStatus, ResearchRoundRecord, WorkflowRun, WorkflowRunFields,
};
use super::stage::ResearchRound;
use super::subquery::{ParallelRetrievalBatch, ParallelRetrievalBatchResult};

/// Legal pure transitions for the multi-hop research state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ResearchTransition {
    /// Decomposing → Retrieving after a plan is accepted.
    StartRetrieval,
    /// Retrieving → EvaluatingCoverage after a batch completes.
    EvaluateCoverage,
    /// EvaluatingCoverage → CorrectiveRound when gaps remain and budget allows.
    StartCorrective,
    /// CorrectiveRound → Retrieving for the corrective batch.
    ResumeRetrieval,
    /// EvaluatingCoverage | CorrectiveRound → Complete.
    Complete,
    /// Any non-terminal → Incomplete.
    FailIncomplete,
}

/// Result of attempting a pure stage advance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RoundAdvance {
    /// Transition applied; new current round.
    Advanced(ResearchRound),
    /// Already terminal; no change.
    AlreadyTerminal(ResearchRound),
}

/// Apply a pure legal transition (no I/O). Fail closed on illegal edges.
pub fn advance_round(
    current: ResearchRound,
    transition: ResearchTransition,
) -> ResearchResult<RoundAdvance> {
    if current.is_terminal() {
        return Ok(RoundAdvance::AlreadyTerminal(current));
    }
    let next = match (current, transition) {
        (ResearchRound::Decomposing, ResearchTransition::StartRetrieval) => {
            ResearchRound::Retrieving
        }
        (ResearchRound::Retrieving, ResearchTransition::EvaluateCoverage) => {
            ResearchRound::EvaluatingCoverage
        }
        (ResearchRound::EvaluatingCoverage, ResearchTransition::StartCorrective) => {
            ResearchRound::CorrectiveRound
        }
        (ResearchRound::CorrectiveRound, ResearchTransition::ResumeRetrieval) => {
            ResearchRound::Retrieving
        }
        (
            ResearchRound::EvaluatingCoverage | ResearchRound::CorrectiveRound,
            ResearchTransition::Complete,
        ) => ResearchRound::Complete,
        (_, ResearchTransition::FailIncomplete) => ResearchRound::Incomplete,
        (from, _) => {
            return Err(ResearchError::illegal_transition(
                from,
                from,
                format!("transition {transition:?} is illegal from {from}"),
            ));
        }
    };
    Ok(RoundAdvance::Advanced(next))
}

/// Decide next action after a coverage report under remaining budget.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CoverageDecision {
    /// Coverage is complete — merge and finish.
    Complete,
    /// Gaps remain and budget allows a corrective round.
    CorrectiveRound,
    /// Gaps remain but budget forbids further rounds — incomplete.
    IncompleteBudget,
    /// Gaps remain and no corrective path — incomplete coverage.
    IncompleteCoverage,
}

/// Pure policy: complete / corrective / incomplete after coverage evaluation.
pub fn decide_after_coverage(
    report: &CoverageReport,
    run: &WorkflowRun,
) -> ResearchResult<CoverageDecision> {
    report.validate()?;
    run.validate()?;
    if report.is_complete {
        return Ok(CoverageDecision::Complete);
    }
    match run.usage.may_start_corrective_round(&run.budget) {
        Ok(()) => Ok(CoverageDecision::CorrectiveRound),
        Err(ResearchError::BudgetExhausted { .. }) => Ok(CoverageDecision::IncompleteBudget),
        Err(err) => Err(err),
    }
}

/// Map a typed error into an incomplete (or disabled) terminal outcome.
pub fn fail_closed(run: &mut WorkflowRun, err: &ResearchError) -> ResearchResult<ResearchOutcome> {
    match err {
        ResearchError::Disabled { detail } => {
            let reason = detail
                .clone()
                .unwrap_or_else(|| "multi-hop research disabled".into());
            run.mark_disabled(reason)?;
            Ok(ResearchOutcome::Disabled { run: run.clone() })
        }
        other => {
            let reason = other.to_string();
            run.mark_incomplete(reason)?;
            Ok(ResearchOutcome::Incomplete { run: run.clone() })
        }
    }
}

/// Try to complete a run with a merged pack after coverage is complete.
pub fn try_complete(
    run: &mut WorkflowRun,
    pack: &MergedContextPack,
) -> ResearchResult<ResearchOutcome> {
    run.complete(pack)?;
    Ok(ResearchOutcome::Complete {
        run: run.clone(),
        merged_context_pack_hash: run
            .merged_context_pack_hash
            .clone()
            .ok_or_else(|| ResearchError::validation("missing merged_context_pack_hash"))?,
    })
}

/// Terminal (or disabled) outcome of multi-hop research.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResearchOutcome {
    Complete {
        run: WorkflowRun,
        merged_context_pack_hash: String,
    },
    Incomplete {
        run: WorkflowRun,
    },
    Disabled {
        run: WorkflowRun,
    },
}

impl ResearchOutcome {
    pub fn run(&self) -> &WorkflowRun {
        match self {
            Self::Complete { run, .. } | Self::Incomplete { run } | Self::Disabled { run } => run,
        }
    }

    pub fn final_status(&self) -> ResearchFinalStatus {
        self.run().final_status
    }
}

/// Trait surface for the bounded multi-hop research workflow.
///
/// Implementations may call retrievers and models **only** through public SDK
/// types. This walking skeleton provides pure contracts; live adapters are
/// residual.
#[async_trait]
pub trait MultiHopResearchWorkflow: Send + Sync {
    /// Create a new run envelope for a research question (no model call).
    fn begin_run(
        &self,
        question: &ResearchQuestion,
        run_id: String,
        budget: ResearchBudget,
    ) -> ResearchResult<WorkflowRun> {
        question.validate()?;
        budget.validate()?;
        let research_question_hash = content_hash_of(question)?;
        WorkflowRun::new(WorkflowRunFields {
            run_id,
            research_question_hash,
            budget,
            profile_ref: None,
            generation: None,
        })
    }

    /// Structured decomposition (adapter residual; trait contract only).
    async fn decompose(
        &self,
        run: &mut WorkflowRun,
        question: &ResearchQuestion,
    ) -> ResearchResult<DecompositionPlan>;

    /// Build + execute one parallel retrieval round.
    async fn retrieve_round(
        &self,
        run: &mut WorkflowRun,
        plan: &DecompositionPlan,
        batch: &ParallelRetrievalBatch,
    ) -> ResearchResult<ParallelRetrievalBatchResult>;

    /// Evaluate coverage / conflicts / unresolved requirements.
    async fn evaluate_coverage(
        &self,
        run: &mut WorkflowRun,
        plan: &DecompositionPlan,
        batch_result: &ParallelRetrievalBatchResult,
    ) -> ResearchResult<CoverageReport>;

    /// Merge/deduplicate into one ContextPack with subquestion attribution.
    async fn merge(
        &self,
        run: &mut WorkflowRun,
        plan: &DecompositionPlan,
        report: &CoverageReport,
    ) -> ResearchResult<MergedContextPack>;

    /// Full pipeline entry (adapters implement; pure helpers above for composition).
    async fn execute(
        &self,
        question: &ResearchQuestion,
        budget: ResearchBudget,
        run_id: String,
    ) -> ResearchResult<ResearchOutcome>;
}

/// Helper: attach a decomposition plan hash after a successful decompose step.
pub fn record_decomposition(run: &mut WorkflowRun, plan: &DecompositionPlan) -> ResearchResult<()> {
    plan.validate()?;
    let hash = content_hash_of(plan)?;
    run.record_round(ResearchRoundRecord {
        round: ResearchRound::Retrieving,
        round_index: 1,
        artifact_hash: Some(hash),
        usage_delta: super::budget::ResearchBudgetUsage {
            rounds: 1,
            ..Default::default()
        },
        ok: true,
        detail: None,
    })?;
    // decomposition_plan_hash is set only when record.round == Decomposing;
    // set explicitly after transitioning to Retrieving.
    run.decomposition_plan_hash = Some(content_hash_of(plan)?);
    run.validate()
}

/// Helper: record coverage evaluation artifact.
pub fn record_coverage(run: &mut WorkflowRun, report: &CoverageReport) -> ResearchResult<()> {
    report.validate()?;
    let hash = content_hash_of(report)?;
    let next = match run.current_round {
        ResearchRound::Retrieving => ResearchRound::EvaluatingCoverage,
        ResearchRound::CorrectiveRound => ResearchRound::EvaluatingCoverage,
        other => {
            return Err(ResearchError::illegal_transition(
                other,
                ResearchRound::EvaluatingCoverage,
                "coverage record requires retrieving or corrective_round",
            ));
        }
    };
    run.record_round(ResearchRoundRecord {
        round: next,
        round_index: report.round_index,
        artifact_hash: Some(hash),
        usage_delta: super::budget::ResearchBudgetUsage::default(),
        ok: true,
        detail: None,
    })
}
