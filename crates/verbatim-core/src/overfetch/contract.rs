//! Ordered bounded retrieval-orchestration trait and stage values.

use std::collections::HashSet;

use serde::{Deserialize, Deserializer, Serialize};

use super::{
    BatchHydrationPort, OverfetchError, OverfetchResult, RetrievalCandidate, RetrievalFilters,
    RetrieverKind, SearchBudget, StatementCountInstrumentation, StrictFilterSupport,
    ValidatedCandidates,
};
use super::{CountPort, HydrationBatch};

/// Explicit diagnostic-output mode. Full output is never the implicit default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiagnosticMode {
    Disabled,
    Compact,
    Full,
}

/// Immutable, serializable plan for one normal bounded retrieval request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct RetrievalPlan {
    budget: SearchBudget,
    filters: RetrievalFilters,
    diagnostic_mode: DiagnosticMode,
}

#[derive(Deserialize)]
struct RetrievalPlanFields {
    budget: SearchBudget,
    filters: RetrievalFilters,
    diagnostic_mode: DiagnosticMode,
}

impl<'de> Deserialize<'de> for RetrievalPlan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = RetrievalPlanFields::deserialize(deserializer)?;
        Self::new(fields.budget, fields.filters)
            .map(|plan| plan.with_diagnostic_mode(fields.diagnostic_mode))
            .map_err(serde::de::Error::custom)
    }
}

impl RetrievalPlan {
    /// Creates a normal-query plan with debugging disabled by default.
    pub fn new(budget: SearchBudget, filters: RetrievalFilters) -> OverfetchResult<Self> {
        let plan = Self {
            budget,
            filters,
            diagnostic_mode: DiagnosticMode::Disabled,
        };
        plan.validate()?;
        Ok(plan)
    }

    /// Explicit opt-in for compact or still-capped full diagnostics.
    pub const fn with_diagnostic_mode(mut self, diagnostic_mode: DiagnosticMode) -> Self {
        self.diagnostic_mode = diagnostic_mode;
        self
    }

    pub const fn budget(&self) -> &SearchBudget {
        &self.budget
    }

    pub fn filters(&self) -> &RetrievalFilters {
        &self.filters
    }

    pub const fn diagnostic_mode(&self) -> DiagnosticMode {
        self.diagnostic_mode
    }

    pub fn validate(&self) -> OverfetchResult<()> {
        self.budget.validate()
    }

    /// Bounded request cap for a strict-filtered retriever attempt.
    pub fn strict_filter_candidate_k(
        &self,
        retriever: RetrieverKind,
        support: &StrictFilterSupport,
        corpus_size: u64,
        attempt: u8,
    ) -> OverfetchResult<u32> {
        let requested = self.budget.candidate_k(retriever);
        if !self.filters.is_strict() {
            return Ok(requested);
        }
        support.candidate_k_for_attempt(requested, requested, corpus_size, attempt)
    }
}

/// Bounded output from the four normal-query retrievers.
#[derive(Debug, Clone, PartialEq)]
pub struct RetrieverCandidates {
    dense: Vec<RetrievalCandidate>,
    lexical: Vec<RetrievalCandidate>,
    exact: Vec<RetrievalCandidate>,
    graph: Vec<RetrievalCandidate>,
}

impl RetrieverCandidates {
    /// Hard-truncates every retriever output before fusion can observe it.
    pub fn new(
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

    pub fn dense(&self) -> &[RetrievalCandidate] {
        &self.dense
    }

    pub fn lexical(&self) -> &[RetrievalCandidate] {
        &self.lexical
    }

    pub fn exact(&self) -> &[RetrievalCandidate] {
        &self.exact
    }

    pub fn graph(&self) -> &[RetrievalCandidate] {
        &self.graph
    }

    pub fn total_len(&self) -> usize {
        self.dense.len() + self.lexical.len() + self.exact.len() + self.graph.len()
    }

    pub(crate) fn truncate_to(mut self, budget: &SearchBudget) -> Self {
        self.dense.truncate(budget.dense_candidate_k as usize);
        self.lexical.truncate(budget.lexical_candidate_k as usize);
        self.exact.truncate(budget.exact_candidate_k as usize);
        self.graph.truncate(budget.graph_candidate_k as usize);
        self
    }
}

/// Deterministically fused and bounded candidate pool.
#[derive(Debug, Clone, PartialEq)]
pub struct FusedCandidates {
    candidates: Vec<RetrievalCandidate>,
}

impl FusedCandidates {
    /// Sorts/deduplicates the already bounded retriever pool, then truncates it.
    ///
    /// The input has at most `SearchBudget::total_retriever_candidates()` items,
    /// so fusion is `O(candidate_budget log candidate_budget)`.
    pub fn fuse_truncate(
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

    pub fn candidates(&self) -> &[RetrievalCandidate] {
        &self.candidates
    }

    pub(crate) fn truncate_to(mut self, budget: &SearchBudget) -> Self {
        self.candidates.truncate(budget.fused_pool_size as usize);
        self
    }
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

/// Pure adapter boundary with a required bounded retrieval orchestration order.
pub trait BoundedRetrievalContract: CountPort + BatchHydrationPort {
    type Report;

    fn plan(
        &self,
        budget: &SearchBudget,
        filters: &RetrievalFilters,
    ) -> OverfetchResult<RetrievalPlan>;

    fn execute_retrievers(&self, plan: &RetrievalPlan) -> OverfetchResult<RetrieverCandidates>;

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

    /// Default final stage: truncate lightweight validations before any complete
    /// fetch and reject oversize completed hydration output.
    fn hydrate_batch(
        &self,
        candidates: ValidatedCandidates,
        plan: &RetrievalPlan,
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<HydrationBatch<Self::Hydrated>> {
        let hydration_input = candidates.for_hydration(plan.budget());
        let hydrated = self.hydrate_full_batch(&hydration_input, statements)?;
        HydrationBatch::new(hydrated, plan.budget())
    }

    fn report(
        &self,
        plan: &RetrievalPlan,
        hydrated: HydrationBatch<Self::Hydrated>,
        statements: &StatementCountInstrumentation,
    ) -> OverfetchResult<Self::Report>;

    /// Executes the required sequence and reapplies each cap at every hand-off.
    fn retrieve(
        &self,
        budget: &SearchBudget,
        filters: &RetrievalFilters,
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Self::Report> {
        budget.validate()?;
        let plan = self.plan(budget, filters)?;
        plan.validate()?;

        let candidates = self.execute_retrievers(&plan)?.truncate_to(plan.budget());
        let fused = self
            .fuse_truncate(candidates, &plan)?
            .truncate_to(plan.budget());
        let validated = self
            .validate_candidates(fused, &plan)?
            .truncate_to(plan.budget());
        let hydrated = self.hydrate_batch(validated, &plan, statements)?;
        self.report(&plan, hydrated, statements)
    }
}

/// Encodes a validated budget without carrying any error detail to callers.
pub fn encode_search_budget_json(budget: &SearchBudget) -> OverfetchResult<String> {
    budget.validate()?;
    serde_json::to_string(budget).map_err(|_| OverfetchError::BudgetExceeded)
}

/// Decodes and validates a budget before it can create retrieval work.
pub fn decode_search_budget_json(input: &str) -> OverfetchResult<SearchBudget> {
    let budget: SearchBudget =
        serde_json::from_str(input).map_err(|_| OverfetchError::BudgetExceeded)?;
    budget.validate()?;
    Ok(budget)
}

/// Encodes a validated plan for a bounded request.
pub fn encode_retrieval_plan_json(plan: &RetrievalPlan) -> OverfetchResult<String> {
    plan.validate()?;
    serde_json::to_string(plan).map_err(|_| OverfetchError::BudgetExceeded)
}

/// Decodes and validates a retrieval plan before it reaches an adapter.
pub fn decode_retrieval_plan_json(input: &str) -> OverfetchResult<RetrievalPlan> {
    let plan: RetrievalPlan =
        serde_json::from_str(input).map_err(|_| OverfetchError::BudgetExceeded)?;
    plan.validate()?;
    Ok(plan)
}
