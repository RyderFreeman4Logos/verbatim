//! Actual-work accounting and remaining-budget derivation.

use super::{
    SearchBudget, SearchBudgetFields, SearchPlannerDiagnosticCode, SearchPlannerError,
    SearchPlannerResult,
};

/// Actual work measured before a public retrieval record is emitted.
///
/// Zero is valid for a usage dimension because a path may not invoke every stage.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SearchBudgetUsage {
    /// Final records returned to the caller.
    pub result_records: u32,
    /// Dense candidates generated.
    pub dense_candidates: u32,
    /// Lexical candidates generated.
    pub lexical_candidates: u32,
    /// Exact-scan candidates generated.
    pub exact_candidates: u32,
    /// Graph candidates generated.
    pub graph_candidates: u32,
    /// Records retained after fusion.
    pub fused_records: u32,
    /// Records admitted to reranking.
    pub reranked_records: u32,
    /// Records rescored at full precision.
    pub full_precision_rescored_records: u32,
    /// Authoritative records hydrated after ranking.
    pub hydrated_records: u32,
    /// SSD pages read.
    pub ssd_pages_read: u64,
    /// Bytes read.
    pub bytes_read: u64,
    /// CPU microseconds consumed.
    pub cpu_micros: u64,
    /// Implementation-defined work units consumed.
    pub work_units: u64,
    /// Shared wall-time consumed in microseconds.
    pub wall_time_micros: u64,
    /// Peak concurrently active retrieval stages.
    pub concurrent_stages: u16,
    /// Stage attempts consumed, including fallbacks.
    pub stage_attempts: u16,
    /// Diagnostic records emitted.
    pub debug_records: u32,
}

impl SearchBudget {
    /// Returns whether actual work remains inside every sealed hard cap.
    pub fn validate_usage(&self, usage: SearchBudgetUsage) -> SearchPlannerResult<()> {
        self.validate()?;
        let fields = self.fields();
        if usage.result_records > fields.result_limit
            || usage.dense_candidates > fields.dense_candidate_limit
            || usage.lexical_candidates > fields.lexical_candidate_limit
            || usage.exact_candidates > fields.exact_candidate_limit
            || usage.graph_candidates > fields.graph_candidate_limit
            || usage.fused_records > fields.fused_pool_limit
            || usage.reranked_records > fields.rerank_candidate_limit
            || usage.full_precision_rescored_records > fields.full_precision_rescore_limit
            || usage.hydrated_records > fields.hydration_limit
            || usage.ssd_pages_read > fields.max_ssd_pages
            || usage.bytes_read > fields.max_bytes_read
            || usage.cpu_micros > fields.max_cpu_micros
            || usage.work_units > fields.max_work_units
            || usage.wall_time_micros > fields.max_wall_time_micros
            || usage.concurrent_stages > fields.max_concurrent_stages
            || usage.stage_attempts > fields.max_stage_attempts
            || usage.debug_records > fields.debug_record_limit
        {
            Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::ActualWorkExceeded,
            ))
        } else {
            Ok(())
        }
    }

    /// Derives a fresh, non-resettable fallback budget from shared consumed work.
    ///
    /// If any required cap is exhausted, construction fails closed instead of
    /// returning a zero or widened fallback budget.
    pub fn remaining_after(&self, usage: SearchBudgetUsage) -> SearchPlannerResult<Self> {
        self.validate_usage(usage)?;
        let fields = self.fields();
        macro_rules! remaining {
            ($cap:expr, $used:expr) => {
                $cap.checked_sub($used).ok_or(SearchPlannerError::new(
                    SearchPlannerDiagnosticCode::FallbackBudgetExhausted,
                ))?
            };
        }
        let remaining = SearchBudgetFields {
            result_limit: remaining!(fields.result_limit, usage.result_records),
            dense_candidate_limit: remaining!(fields.dense_candidate_limit, usage.dense_candidates),
            lexical_candidate_limit: remaining!(
                fields.lexical_candidate_limit,
                usage.lexical_candidates
            ),
            exact_candidate_limit: remaining!(fields.exact_candidate_limit, usage.exact_candidates),
            graph_candidate_limit: remaining!(fields.graph_candidate_limit, usage.graph_candidates),
            fused_pool_limit: remaining!(fields.fused_pool_limit, usage.fused_records),
            rerank_candidate_limit: remaining!(
                fields.rerank_candidate_limit,
                usage.reranked_records
            ),
            full_precision_rescore_limit: remaining!(
                fields.full_precision_rescore_limit,
                usage.full_precision_rescored_records
            ),
            hydration_limit: remaining!(fields.hydration_limit, usage.hydrated_records),
            max_ssd_pages: remaining!(fields.max_ssd_pages, usage.ssd_pages_read),
            max_bytes_read: remaining!(fields.max_bytes_read, usage.bytes_read),
            max_cpu_micros: remaining!(fields.max_cpu_micros, usage.cpu_micros),
            max_work_units: remaining!(fields.max_work_units, usage.work_units),
            max_wall_time_micros: remaining!(fields.max_wall_time_micros, usage.wall_time_micros),
            max_concurrent_stages: remaining!(
                fields.max_concurrent_stages,
                usage.concurrent_stages
            ),
            max_stage_attempts: remaining!(fields.max_stage_attempts, usage.stage_attempts),
            debug_record_limit: remaining!(fields.debug_record_limit, usage.debug_records),
        };
        Self::new(remaining).map_err(|_| {
            SearchPlannerError::new(SearchPlannerDiagnosticCode::FallbackBudgetExhausted)
        })
    }
}
