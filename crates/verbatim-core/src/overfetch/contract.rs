//! Atomic bounded retrieval orchestration.
//!
//! The public contract exposes one sealed entry point. Planning, count gating,
//! retriever execution, candidate stages, and complete hydration remain
//! crate-internal so callers cannot skip a boundary or substitute a looser
//! intermediate plan.

use std::{collections::HashSet, fmt};

use serde::{Deserialize, Serialize};

use super::backend::{
    PrimaryBackendOutcome, PrimaryBackendSelection, RetrievalBackend, TypedBackendFailure,
};
use super::budget::SearchBudget;
use super::count::CountPort;
use super::error::{OverfetchError, OverfetchResult};
use super::hydration::{BatchHydrationPort, HydrationBatch};
use super::instrumentation::StatementCountInstrumentation;
use super::policy::{
    RetrievalCandidate, RetrievalFilters, StrictFilterSupport, ValidatedCandidates,
};

/// Internal marker that prevents external implementations from bypassing the
/// bounded orchestration pipeline.
pub(crate) mod sealed {
    pub trait Sealed {}
}

/// Serializes a validated normal-query budget for an explicit caller boundary.
pub fn encode_search_budget_json(budget: &SearchBudget) -> OverfetchResult<String> {
    budget.validate()?;
    serde_json::to_string(budget).map_err(|_| OverfetchError::BudgetExceeded)
}

/// Decodes and revalidates an untrusted normal-query budget.
pub fn decode_search_budget_json(input: &str) -> OverfetchResult<SearchBudget> {
    let budget =
        serde_json::from_str::<SearchBudget>(input).map_err(|_| OverfetchError::BudgetExceeded)?;
    budget.validate()?;
    Ok(budget)
}

/// The only normal-retrieval entry point.
///
/// Implementations are sealed to this crate. Every invocation validates the
/// caller request, creates and revalidates an internal plan, counts strict
/// filters before a retriever request, executes the selected primary first,
/// and verifies complete hydration instrumentation before reporting.
pub(crate) trait BoundedRetrievalContract: sealed::Sealed {
    type Report;

    fn retrieve(
        &self,
        budget: &SearchBudget,
        filters: &RetrievalFilters,
        strict_filter_support: &StrictFilterSupport,
        primary_backend_selection: &PrimaryBackendSelection,
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Self::Report>;
}

/// Crate-only adapter hooks invoked by the sealed bounded contract.
pub(crate) trait RetrievalAdapter: CountPort + BatchHydrationPort + sealed::Sealed {
    type Report;

    fn plan(
        &self,
        budget: &SearchBudget,
        filters: &RetrievalFilters,
        strict_filter_support: &StrictFilterSupport,
        primary_backend_selection: &PrimaryBackendSelection,
    ) -> OverfetchResult<RetrievalPlan>;

    fn execute_retrievers(
        &self,
        plan: &RetrievalPlan,
        backend: RetrievalBackend,
    ) -> OverfetchResult<PrimaryBackendAttempt>;

    fn fuse_truncate(
        &self,
        candidates: RetrieverCandidates,
        plan: &RetrievalPlan,
    ) -> OverfetchResult<FusedCandidates>;

    fn validate_candidates(
        &self,
        candidates: FusedCandidates,
        plan: &RetrievalPlan,
    ) -> OverfetchResult<ValidatedCandidates>;

    /// Uses the complete-hydration port by default, after final candidate
    /// truncation. Adapter overrides are still checked by the caller-visible
    /// complete-batch instrumentation assertion.
    fn hydrate_batch(
        &self,
        candidates: ValidatedCandidates,
        plan: &RetrievalPlan,
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<HydrationBatch<Self::Hydrated>> {
        let hydration_candidates = candidates.for_hydration(plan.budget());
        let hydrated = self.hydrate_full_batch(&hydration_candidates, statements)?;
        HydrationBatch::new(hydrated, plan.budget())
    }

    fn report(
        &self,
        plan: &RetrievalPlan,
        hydrated: HydrationBatch<Self::Hydrated>,
        statements: &StatementCountInstrumentation,
    ) -> OverfetchResult<Self::Report>;
}

impl<T> BoundedRetrievalContract for T
where
    T: RetrievalAdapter,
{
    type Report = T::Report;

    fn retrieve(
        &self,
        budget: &SearchBudget,
        filters: &RetrievalFilters,
        strict_filter_support: &StrictFilterSupport,
        primary_backend_selection: &PrimaryBackendSelection,
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Self::Report> {
        statements.assert_fresh()?;
        budget.validate()?;
        primary_backend_selection.validate()?;

        let planned = self.plan(
            budget,
            filters,
            strict_filter_support,
            primary_backend_selection,
        )?;
        planned.validate_for_request(
            budget,
            filters,
            strict_filter_support,
            primary_backend_selection,
        )?;

        // Strict filtering must be evaluated against a bounded indexed count
        // before a retriever receives an effective candidate cap.
        let corpus_size = self.count_indexed(planned.filters())?;
        let plan = planned.with_effective_strict_filter_budget(corpus_size)?;

        let retrieved = execute_primary_then_optional_typed_fallback(self, &plan)?;
        let fused = self.fuse_truncate(retrieved, &plan)?;
        let validated = self.validate_candidates(fused, &plan)?;
        let hydrated = self.hydrate_batch(validated, &plan, statements)?;

        // A duplicate batch is caught when recorded. This check additionally
        // rejects omitted batches and unclassified per-candidate statements.
        statements.assert_complete_batched_hydration()?;
        self.report(&plan, hydrated, statements)
    }
}

fn execute_primary_then_optional_typed_fallback<T>(
    adapter: &T,
    plan: &RetrievalPlan,
) -> OverfetchResult<RetrieverCandidates>
where
    T: RetrievalAdapter,
{
    let selection = *plan.primary_backend_selection();
    let primary = selection.primary();
    selection.validate_first_attempt(primary)?;

    match adapter.execute_retrievers(plan, primary)? {
        PrimaryBackendAttempt::Satisfied(candidates) => Ok(candidates),
        PrimaryBackendAttempt::DeclaredInsufficientResults => {
            // Deliberately invoke the selection policy here so this path cannot
            // accidentally grow a direct fallback in future orchestration code.
            selection.fallback_after(PrimaryBackendOutcome::DeclaredInsufficientResults)?;
            Err(OverfetchError::PrimaryBackendRequired)
        }
        PrimaryBackendAttempt::TypedFailure(failure) => {
            let fallback = selection
                .fallback_after(PrimaryBackendOutcome::TypedFailure(failure))?
                .ok_or(OverfetchError::PrimaryBackendRequired)?;
            match adapter.execute_retrievers(plan, fallback)? {
                PrimaryBackendAttempt::Satisfied(candidates) => Ok(candidates),
                PrimaryBackendAttempt::DeclaredInsufficientResults
                | PrimaryBackendAttempt::TypedFailure(_) => {
                    Err(OverfetchError::PrimaryBackendRequired)
                }
            }
        }
    }
}

/// Outcome of one selected backend attempt.
#[derive(Debug, Clone, PartialEq)]
pub(crate) enum PrimaryBackendAttempt {
    Satisfied(RetrieverCandidates),
    DeclaredInsufficientResults,
    TypedFailure(TypedBackendFailure),
}

/// Internal plan that cannot be serialized or supplied directly by callers.
#[derive(Clone, PartialEq, Eq)]
pub(crate) struct RetrievalPlan {
    budget: SearchBudget,
    filters: RetrievalFilters,
    strict_filter_support: StrictFilterSupport,
    primary_backend_selection: PrimaryBackendSelection,
    diagnostic_mode: DiagnosticMode,
}

impl RetrievalPlan {
    pub(crate) fn new(
        budget: SearchBudget,
        filters: RetrievalFilters,
        strict_filter_support: StrictFilterSupport,
        primary_backend_selection: PrimaryBackendSelection,
    ) -> OverfetchResult<Self> {
        budget.validate()?;
        primary_backend_selection.validate()?;
        Ok(Self {
            budget,
            filters,
            strict_filter_support,
            primary_backend_selection,
            diagnostic_mode: DiagnosticMode::Disabled,
        })
    }

    pub(crate) fn budget(&self) -> &SearchBudget {
        &self.budget
    }

    pub(crate) fn filters(&self) -> &RetrievalFilters {
        &self.filters
    }

    pub(crate) fn strict_filter_support(&self) -> &StrictFilterSupport {
        &self.strict_filter_support
    }

    pub(crate) fn primary_backend_selection(&self) -> &PrimaryBackendSelection {
        &self.primary_backend_selection
    }

    fn validate_for_request(
        &self,
        requested_budget: &SearchBudget,
        requested_filters: &RetrievalFilters,
        requested_strict_filter_support: &StrictFilterSupport,
        requested_primary_backend_selection: &PrimaryBackendSelection,
    ) -> OverfetchResult<()> {
        self.budget.validate()?;
        self.primary_backend_selection.validate()?;
        if !self.budget.is_within(requested_budget) {
            return Err(OverfetchError::BudgetExceeded);
        }
        if !self.filters.preserves(requested_filters)
            || self.strict_filter_support != *requested_strict_filter_support
        {
            return Err(OverfetchError::UnsupportedStrictFilter);
        }
        if self.primary_backend_selection != *requested_primary_backend_selection {
            return Err(OverfetchError::PrimaryBackendRequired);
        }
        Ok(())
    }

    fn with_effective_strict_filter_budget(mut self, corpus_size: u64) -> OverfetchResult<Self> {
        if !self.filters.is_strict() {
            return Ok(self);
        }

        self.budget = self.budget.with_retriever_candidate_ks(
            self.strict_filter_support.candidate_k_for_attempt(
                self.budget.dense_candidate_k,
                self.budget.dense_candidate_k,
                corpus_size,
                0,
            )?,
            self.strict_filter_support.candidate_k_for_attempt(
                self.budget.lexical_candidate_k,
                self.budget.lexical_candidate_k,
                corpus_size,
                0,
            )?,
            self.strict_filter_support.candidate_k_for_attempt(
                self.budget.exact_candidate_k,
                self.budget.exact_candidate_k,
                corpus_size,
                0,
            )?,
            self.strict_filter_support.candidate_k_for_attempt(
                self.budget.graph_candidate_k,
                self.budget.graph_candidate_k,
                corpus_size,
                0,
            )?,
        )?;
        Ok(self)
    }
}

impl fmt::Debug for RetrievalPlan {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RetrievalPlan")
            .field("budget", &self.budget)
            .field("filters", &self.filters)
            .field("strict_filter_support", &self.strict_filter_support)
            .field("primary_backend_selection", &self.primary_backend_selection)
            .field("diagnostic_mode", &self.diagnostic_mode)
            .finish()
    }
}

/// Candidate lists returned by independently bounded retrievers.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct RetrieverCandidates {
    dense: Vec<RetrievalCandidate>,
    lexical: Vec<RetrievalCandidate>,
    exact: Vec<RetrievalCandidate>,
    graph: Vec<RetrievalCandidate>,
}

impl RetrieverCandidates {
    pub(crate) fn new(
        budget: &SearchBudget,
        mut dense: Vec<RetrievalCandidate>,
        mut lexical: Vec<RetrievalCandidate>,
        mut exact: Vec<RetrievalCandidate>,
        mut graph: Vec<RetrievalCandidate>,
    ) -> OverfetchResult<Self> {
        budget.validate()?;
        dense.truncate(budget.dense_candidate_k as usize);
        lexical.truncate(budget.lexical_candidate_k as usize);
        exact.truncate(budget.exact_candidate_k as usize);
        graph.truncate(budget.graph_candidate_k as usize);
        Ok(Self {
            dense,
            lexical,
            exact,
            graph,
        })
    }

    pub(crate) fn dense(&self) -> &[RetrievalCandidate] {
        &self.dense
    }

    pub(crate) fn lexical(&self) -> &[RetrievalCandidate] {
        &self.lexical
    }

    pub(crate) fn exact(&self) -> &[RetrievalCandidate] {
        &self.exact
    }

    pub(crate) fn graph(&self) -> &[RetrievalCandidate] {
        &self.graph
    }
}

/// Fused candidate collection, hard-truncated before validation or reranking.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct FusedCandidates {
    candidates: Vec<RetrievalCandidate>,
}

impl FusedCandidates {
    pub(crate) fn fuse_truncate(
        candidates: RetrieverCandidates,
        budget: &SearchBudget,
    ) -> OverfetchResult<Self> {
        budget.validate()?;
        let mut all = Vec::new();
        all.extend(candidates.dense);
        all.extend(candidates.lexical);
        all.extend(candidates.exact);
        all.extend(candidates.graph);
        all.sort_by(|left, right| {
            right
                .score()
                .total_cmp(&left.score())
                .then_with(|| left.id().as_str().cmp(right.id().as_str()))
        });

        let mut seen = HashSet::new();
        all.retain(|candidate| seen.insert(candidate.id().clone()));
        all.truncate(budget.fused_pool_size as usize);
        Ok(Self { candidates: all })
    }

    pub(crate) fn candidates(&self) -> &[RetrievalCandidate] {
        &self.candidates
    }
}

/// Opt-in bounded diagnostic collection mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticMode {
    Disabled,
    Compact,
    Full,
}

/// Bounded diagnostic output collected only in an explicit enabled mode.
#[derive(Debug, Clone, PartialEq)]
pub struct DebugOutput<T> {
    mode: DiagnosticMode,
    entries: Vec<T>,
}

impl<T> DebugOutput<T> {
    /// Lazily creates compact or explicit-full diagnostics under their own cap.
    ///
    /// `entries` is not invoked when diagnostics are disabled, preventing a
    /// disabled debug path from materializing any candidates.
    pub fn collect<F, I>(
        mode: DiagnosticMode,
        budget: &SearchBudget,
        entries: F,
    ) -> OverfetchResult<Self>
    where
        F: FnOnce() -> I,
        I: IntoIterator<Item = T>,
    {
        budget.validate()?;
        match mode {
            DiagnosticMode::Disabled => Ok(Self {
                mode,
                entries: Vec::new(),
            }),
            DiagnosticMode::Compact | DiagnosticMode::Full => Ok(Self {
                mode,
                entries: entries()
                    .into_iter()
                    .take(budget.debug_output_size as usize)
                    .collect(),
            }),
        }
    }

    pub const fn mode(&self) -> DiagnosticMode {
        self.mode
    }

    pub fn entries(&self) -> &[T] {
        &self.entries
    }
}

/// Public complexity notation for the normal bounded workflow.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ComplexityInvariant;

impl ComplexityInvariant {
    pub const RETRIEVER_CANDIDATES: &'static str = "O(k_dense + k_lexical + k_exact + k_graph)";
    pub const FUSION: &'static str = "O(candidate_budget log candidate_budget)";
    pub const HYDRATION_SQL_CALLS: &'static str = "O(1) batches";
    pub const HYDRATED_TEXT: &'static str = "O(final_limit + bounded_rerank_candidates)";
}
