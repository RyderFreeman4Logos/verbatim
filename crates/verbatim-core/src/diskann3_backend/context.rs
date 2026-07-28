//! Validated generation, predicate, and budget context for adapter operations.

use std::fmt;

use crate::diskann3::{FilterPredicate, PublicationGeneration};
use crate::search_planner::SearchBudget;

use super::{
    DiskAnnBackendDiagnosticCode, DiskAnnBackendError, DiskAnnBackendResult, VectorInput,
    VectorSpaceSpec,
};

/// A caller budget paired with the equal-or-narrower operation budget consumed by the adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchBudgetBinding {
    caller_budget: SearchBudget,
    operation_budget: SearchBudget,
}

impl SearchBudgetBinding {
    /// Rejects malformed budgets and any operation budget wider than the caller's authority.
    pub fn new(
        caller_budget: SearchBudget,
        operation_budget: SearchBudget,
    ) -> DiskAnnBackendResult<Self> {
        caller_budget.validate().map_err(|_| {
            DiskAnnBackendError::contract(DiskAnnBackendDiagnosticCode::InvalidSearchBudget)
        })?;
        operation_budget.validate().map_err(|_| {
            DiskAnnBackendError::contract(DiskAnnBackendDiagnosticCode::InvalidSearchBudget)
        })?;
        if !operation_budget.is_not_wider_than(&caller_budget) {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::SearchBudgetWidened,
            ));
        }
        Ok(Self {
            caller_budget,
            operation_budget,
        })
    }

    /// Returns the outer caller-authorized budget.
    pub const fn caller_budget(&self) -> SearchBudget {
        self.caller_budget
    }

    /// Returns the adapter operation's equal-or-narrower budget.
    pub const fn operation_budget(&self) -> SearchBudget {
        self.operation_budget
    }
}

/// An immutable binding for one published vector-space generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationContext {
    vector_space: VectorSpaceSpec,
    generation: PublicationGeneration,
    budget_binding: SearchBudgetBinding,
}

impl GenerationContext {
    /// Creates a validated generation-scoped operation context.
    pub fn new(
        vector_space: VectorSpaceSpec,
        generation: PublicationGeneration,
        budget_binding: SearchBudgetBinding,
    ) -> DiskAnnBackendResult<Self> {
        vector_space.vector_space_id().validate().map_err(|_| {
            DiskAnnBackendError::contract(DiskAnnBackendDiagnosticCode::InvalidGenerationContext)
        })?;
        if generation.value() == 0 {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidGenerationContext,
            ));
        }
        Ok(Self {
            vector_space,
            generation,
            budget_binding,
        })
    }

    /// Returns the validated vector-space specification.
    pub const fn vector_space(&self) -> &VectorSpaceSpec {
        &self.vector_space
    }

    /// Returns the immutable publication generation.
    pub const fn generation(&self) -> PublicationGeneration {
        self.generation
    }

    /// Returns the bounded caller/operation budget pair.
    pub const fn budget_binding(&self) -> &SearchBudgetBinding {
        &self.budget_binding
    }

    /// Validates a vector against this context's profile and publication generation.
    pub fn validate_input(&self, input: &VectorInput) -> DiskAnnBackendResult<()> {
        self.vector_space.validate_input(input, self.generation)
    }
}

/// A bounded filter plan that must be applied during candidate generation.
#[derive(Clone, PartialEq)]
pub struct PredicatePlan {
    filters: Vec<FilterPredicate>,
}

impl PredicatePlan {
    /// Prevents unbounded filter payloads from crossing the adapter boundary.
    pub const MAX_FILTERS: usize = 16;

    /// Validates all predicates before an adapter can begin ANN traversal.
    pub fn new(filters: Vec<FilterPredicate>) -> DiskAnnBackendResult<Self> {
        if filters.len() > Self::MAX_FILTERS {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidPredicatePlan,
            ));
        }
        for filter in &filters {
            filter.validate().map_err(|_| {
                DiskAnnBackendError::contract(DiskAnnBackendDiagnosticCode::InvalidPredicatePlan)
            })?;
        }
        Ok(Self { filters })
    }

    /// Returns the only filters that may be used to produce candidates.
    pub fn filters(&self) -> &[FilterPredicate] {
        &self.filters
    }

    /// Signals that adapter implementations must not defer this filter to post-ranking.
    pub const fn requires_candidate_filtering(&self) -> bool {
        true
    }
}

impl fmt::Debug for PredicatePlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("PredicatePlan(REDACTED)")
    }
}

/// Generation context plus the predicate plan required for a search operation.
#[derive(Clone, PartialEq)]
pub struct SearchContext {
    generation: GenerationContext,
    predicate: PredicatePlan,
}

impl SearchContext {
    /// Creates a search context whose predicate applies before candidate output.
    pub fn new(
        generation: GenerationContext,
        predicate: PredicatePlan,
    ) -> DiskAnnBackendResult<Self> {
        if !predicate.requires_candidate_filtering() {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidPredicatePlan,
            ));
        }
        Ok(Self {
            generation,
            predicate,
        })
    }

    /// Returns the generation-bound portion of the context.
    pub const fn generation(&self) -> &GenerationContext {
        &self.generation
    }

    /// Returns the predicate that must constrain candidate generation.
    pub const fn predicate(&self) -> &PredicatePlan {
        &self.predicate
    }
}

impl fmt::Debug for SearchContext {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("SearchContext")
            .field("generation", &self.generation)
            .field("predicate", &"REDACTED")
            .finish()
    }
}
