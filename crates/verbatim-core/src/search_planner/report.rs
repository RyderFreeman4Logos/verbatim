//! Public execution records that preserve plan binding and partial state.

use super::{
    GenerationBinding, PlanIdentity, RetrievalPlan, SearchBudget, SearchBudgetUsage,
    SearchPlannerDiagnosticCode, SearchPlannerError, SearchPlannerResult,
};

/// Typed completion state attached to every public retrieval record.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CompletionState {
    /// All planned work completed inside the sealed budget.
    Complete,
    /// A hard budget prevented further work.
    BudgetExhausted,
    /// The shared wall-time deadline prevented further work.
    DeadlineExceeded,
    /// A strict predicate could not be enforced by the backend.
    UnsupportedStrictFilter,
    /// Approximate candidate generation or a named degraded profile is visible.
    ApproximatePartial,
}

impl CompletionState {
    /// Returns whether this state must be presented as partial.
    pub const fn is_partial(self) -> bool {
        !matches!(self, Self::Complete)
    }
}

/// Public, non-sensitive retrieval artifact emitted after execution or reporting.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PublicRetrievalRecord {
    plan_identity: PlanIdentity,
    generation: GenerationBinding,
    budget: SearchBudget,
    actual_work: SearchBudgetUsage,
    completion: CompletionState,
}

impl PublicRetrievalRecord {
    pub(crate) const fn new(
        plan_identity: PlanIdentity,
        generation: GenerationBinding,
        budget: SearchBudget,
        actual_work: SearchBudgetUsage,
        completion: CompletionState,
    ) -> Self {
        Self {
            plan_identity,
            generation,
            budget,
            actual_work,
            completion,
        }
    }

    /// Returns the originating opaque plan identity.
    pub const fn plan_identity(&self) -> PlanIdentity {
        self.plan_identity
    }

    /// Returns the immutable generation binding.
    pub const fn generation(&self) -> GenerationBinding {
        self.generation
    }

    /// Returns the sealed budget snapshot.
    pub const fn budget(&self) -> SearchBudget {
        self.budget
    }

    /// Returns the measured actual work checked against the sealed budget.
    pub const fn actual_work(&self) -> SearchBudgetUsage {
        self.actual_work
    }

    /// Returns the visible typed completion state.
    pub const fn completion(&self) -> CompletionState {
        self.completion
    }
}

impl RetrievalPlan {
    /// Produces a public record only after actual work fits the sealed budget.
    pub fn record_actual_work(
        &self,
        actual_work: SearchBudgetUsage,
        completion: CompletionState,
    ) -> SearchPlannerResult<PublicRetrievalRecord> {
        self.budget().validate_usage(actual_work)?;
        if self.planned_completeness().is_partial() && completion == CompletionState::Complete {
            return Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::InvalidPlan,
            ));
        }
        Ok(PublicRetrievalRecord::new(
            self.identity(),
            self.generation(),
            self.budget(),
            actual_work,
            completion,
        ))
    }

    /// Rejects a fallback that exceeds the remaining shared deadline or work caps.
    pub fn validate_fallback_budget(
        &self,
        consumed: SearchBudgetUsage,
        fallback: &SearchBudget,
    ) -> SearchPlannerResult<()> {
        let remaining = self.budget().remaining_after(consumed)?;
        fallback.ensure_not_wider_than(&remaining)
    }
}
