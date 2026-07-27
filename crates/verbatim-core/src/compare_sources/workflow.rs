//! Pure state-machine helpers and adapter trait for compare-sources.

use async_trait::async_trait;

use super::budget::ComparisonBudget;
use super::dimension::{ComparisonDimension, DimensionValue};
use super::error::{ComparisonError, ComparisonResultType};
use super::result::{ComparisonContextPack, ComparisonResult};
use super::run::{
    content_hash_of, CompareSourcesWorkflowRun, CompareSourcesWorkflowRunFields,
    ComparisonRunStatus, ComparisonStageRecord,
};
use super::scope::ComparisonScope;
use super::stage::ComparisonStage;

/// Legal state-machine edge for the pure workflow helpers.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComparisonTransition {
    pub from: ComparisonStage,
    pub to: ComparisonStage,
}

/// Result of requesting a non-terminal stage advance.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StageAdvance {
    Advanced(ComparisonTransition),
    AlreadyTerminal(ComparisonStage),
}

/// Terminal adapter outcome. A partial, ungrounded comparison is never Complete.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComparisonOutcome {
    Complete(Box<ComparisonContextPack>),
    Incomplete(ComparisonError),
    Disabled(ComparisonError),
}

/// Advance one legal non-terminal stage. It does not make a result publishable;
/// [`try_complete`] requires final rendered artifact hashes.
pub fn advance_stage(
    run: &mut CompareSourcesWorkflowRun,
    to: ComparisonStage,
) -> ComparisonResultType<StageAdvance> {
    if run.status.is_terminal() || run.current_stage.is_terminal() {
        return Ok(StageAdvance::AlreadyTerminal(run.current_stage));
    }
    if !is_legal_advance(run.current_stage, to) {
        return Err(ComparisonError::IllegalTransition {
            detail: format!("cannot advance {} to {}", run.current_stage, to),
        });
    }
    let transition = ComparisonTransition {
        from: run.current_stage,
        to,
    };
    run.current_stage = to;
    Ok(StageAdvance::Advanced(transition))
}

fn is_legal_advance(from: ComparisonStage, to: ComparisonStage) -> bool {
    matches!(
        (from, to),
        (ComparisonStage::Decomposing, ComparisonStage::Resolving)
            | (ComparisonStage::Resolving, ComparisonStage::Extracting)
            | (ComparisonStage::Extracting, ComparisonStage::Aligning)
            | (ComparisonStage::Aligning, ComparisonStage::Rendering)
    )
}

/// Complete only after rendering and hashes for both result and reusable pack.
pub fn try_complete(
    run: &mut CompareSourcesWorkflowRun,
    result: &ComparisonResult,
    pack: &ComparisonContextPack,
) -> ComparisonResultType<ComparisonOutcome> {
    if run.status.is_terminal() {
        return Err(ComparisonError::IllegalTransition {
            detail: "cannot complete a terminal comparison run".into(),
        });
    }
    if run.current_stage != ComparisonStage::Rendering {
        return Err(ComparisonError::IllegalTransition {
            detail: "comparison completion requires rendering stage".into(),
        });
    }
    let result_hash = content_hash_of(result)?;
    let pack_hash = content_hash_of(pack)?;
    run.complete(result_hash, pack_hash)?;
    Ok(ComparisonOutcome::Complete(Box::new(pack.clone())))
}

/// Turn an adapter error into a terminal, fail-closed outcome and run status.
pub fn fail_closed(
    run: &mut CompareSourcesWorkflowRun,
    error: ComparisonError,
) -> ComparisonResultType<ComparisonOutcome> {
    match error {
        error @ ComparisonError::Disabled { .. } => {
            run.mark_disabled(error.to_string())?;
            Ok(ComparisonOutcome::Disabled(error))
        }
        error => {
            run.mark_incomplete(error.to_string())?;
            Ok(ComparisonOutcome::Incomplete(error))
        }
    }
}

/// Adapter surface only. Implementations may use public SDK/pagination/wire
/// contracts, but must preserve this module's ACL, evidence, budget, and stage
/// invariants. This contract has no live implementation.
#[async_trait]
pub trait CompareSourcesWorkflow: Send + Sync {
    /// Break the question/scope into bounded comparison dimensions.
    async fn decompose(
        &self,
        run: &mut CompareSourcesWorkflowRun,
        scope: &ComparisonScope,
    ) -> ComparisonResultType<Vec<ComparisonDimension>>;

    /// Resolve both requested source versions and enforce ACL/lifecycle rules.
    async fn resolve(
        &self,
        run: &mut CompareSourcesWorkflowRun,
        scope: &ComparisonScope,
    ) -> ComparisonResultType<ComparisonScope>;

    /// Extract provenance-bound quotations and optional separated interpretations.
    async fn extract(
        &self,
        run: &mut CompareSourcesWorkflowRun,
        scope: &ComparisonScope,
        dimensions: &[ComparisonDimension],
    ) -> ComparisonResultType<Vec<DimensionValue>>;

    /// Align two sides into structured cells with an explicit alignment class.
    async fn align(
        &self,
        run: &mut CompareSourcesWorkflowRun,
        scope: &ComparisonScope,
        dimensions: &[ComparisonDimension],
        values: Vec<DimensionValue>,
    ) -> ComparisonResultType<ComparisonResult>;

    /// Render a reusable comparison ContextPack, retaining source quotations.
    async fn render(
        &self,
        run: &mut CompareSourcesWorkflowRun,
        scope: &ComparisonScope,
        result: ComparisonResult,
    ) -> ComparisonResultType<ComparisonContextPack>;
}

/// Begin a run only after scope resolution/ACL status and hard budget checks.
pub fn start_run(
    run_id: String,
    scope: &ComparisonScope,
    budget: ComparisonBudget,
) -> ComparisonResultType<CompareSourcesWorkflowRun> {
    scope.require_comparable()?;
    let run = CompareSourcesWorkflowRun::new(CompareSourcesWorkflowRunFields {
        run_id,
        scope_hash: content_hash_of(scope)?,
        budget,
    })?;
    if run.status != ComparisonRunStatus::Running {
        return Err(ComparisonError::validation(
            "new comparison run must be running",
        ));
    }
    Ok(run)
}

/// Record an artifact after its owning stage has been entered. Budget accounting
/// is checked before mutation by [`CompareSourcesWorkflowRun::record_stage`].
pub fn record_stage(
    run: &mut CompareSourcesWorkflowRun,
    record: ComparisonStageRecord,
) -> ComparisonResultType<()> {
    if run.status.is_terminal() {
        return Err(ComparisonError::IllegalTransition {
            detail: "cannot record a terminal comparison run".into(),
        });
    }
    if record.stage != run.current_stage {
        return Err(ComparisonError::IllegalTransition {
            detail: format!(
                "stage record {} does not match active stage {}",
                record.stage, run.current_stage
            ),
        });
    }
    run.record_stage(record)
}
