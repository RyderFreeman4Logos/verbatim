//! Hard, independently accounted limits for a retrieval plan.

use super::{SearchPlannerDiagnosticCode, SearchPlannerError, SearchPlannerResult};

/// Field bag used to construct and validate a [`SearchBudget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchBudgetFields {
    /// Maximum final records visible to the caller.
    pub result_limit: u32,
    /// Maximum dense candidate records generated before fusion.
    pub dense_candidate_limit: u32,
    /// Maximum lexical candidate records generated before fusion.
    pub lexical_candidate_limit: u32,
    /// Maximum exact-scan candidate records generated before fusion.
    pub exact_candidate_limit: u32,
    /// Maximum graph candidate records generated before fusion.
    pub graph_candidate_limit: u32,
    /// Maximum records retained after fusion.
    pub fused_pool_limit: u32,
    /// Maximum records admitted to reranking.
    pub rerank_candidate_limit: u32,
    /// Maximum full-precision candidate records admitted to rescoring.
    pub full_precision_rescore_limit: u32,
    /// Maximum authoritative records hydrated after ranking.
    pub hydration_limit: u32,
    /// Maximum SSD pages read by the plan.
    pub max_ssd_pages: u64,
    /// Maximum bytes read by the plan.
    pub max_bytes_read: u64,
    /// Maximum CPU microseconds consumed by the plan.
    pub max_cpu_micros: u64,
    /// Maximum implementation-defined work units consumed by the plan.
    pub max_work_units: u64,
    /// Maximum shared wall-time deadline in microseconds.
    pub max_wall_time_micros: u64,
    /// Maximum concurrently active retrieval stages.
    pub max_concurrent_stages: u16,
    /// Maximum stage attempts, including any fallback attempt.
    pub max_stage_attempts: u16,
    /// Maximum diagnostic records, independently capped from visible results.
    pub debug_record_limit: u32,
}

/// Validated hard bounds for one authorized retrieval request.
///
/// All limits are positive. A plan may narrow these values but may never widen them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchBudget {
    fields: SearchBudgetFields,
}

impl SearchBudget {
    /// Constructs a budget only when every independent cap is positive and ordered.
    pub fn new(fields: SearchBudgetFields) -> SearchPlannerResult<Self> {
        let budget = Self { fields };
        budget.validate()?;
        Ok(budget)
    }

    /// Returns the validated field bag for inspection or a narrower derived budget.
    pub const fn fields(&self) -> SearchBudgetFields {
        self.fields
    }

    /// Revalidates budget invariants before any adapter creates work.
    pub fn validate(&self) -> SearchPlannerResult<()> {
        let fields = self.fields;
        if [
            fields.result_limit,
            fields.dense_candidate_limit,
            fields.lexical_candidate_limit,
            fields.exact_candidate_limit,
            fields.graph_candidate_limit,
            fields.fused_pool_limit,
            fields.rerank_candidate_limit,
            fields.full_precision_rescore_limit,
            fields.hydration_limit,
            fields.debug_record_limit,
        ]
        .contains(&0)
            || [
                fields.max_ssd_pages,
                fields.max_bytes_read,
                fields.max_cpu_micros,
                fields.max_work_units,
                fields.max_wall_time_micros,
            ]
            .contains(&0)
            || [fields.max_concurrent_stages, fields.max_stage_attempts].contains(&0)
            || fields.result_limit > fields.hydration_limit
            || fields.hydration_limit > fields.rerank_candidate_limit
            || fields.rerank_candidate_limit > fields.fused_pool_limit
            || fields.full_precision_rescore_limit > fields.rerank_candidate_limit
        {
            return Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::BudgetExceeded,
            ));
        }
        self.total_candidate_limit().map(|_| ())
    }

    /// Returns the checked sum of all independently bounded candidate sources.
    pub fn total_candidate_limit(&self) -> SearchPlannerResult<u32> {
        self.fields
            .dense_candidate_limit
            .checked_add(self.fields.lexical_candidate_limit)
            .and_then(|sum| sum.checked_add(self.fields.exact_candidate_limit))
            .and_then(|sum| sum.checked_add(self.fields.graph_candidate_limit))
            .ok_or(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::BudgetOverflow,
            ))
    }

    /// Returns whether every hard cap is no greater than the caller's cap.
    pub const fn is_not_wider_than(&self, caller: &Self) -> bool {
        let plan = self.fields;
        let request = caller.fields;
        plan.result_limit <= request.result_limit
            && plan.dense_candidate_limit <= request.dense_candidate_limit
            && plan.lexical_candidate_limit <= request.lexical_candidate_limit
            && plan.exact_candidate_limit <= request.exact_candidate_limit
            && plan.graph_candidate_limit <= request.graph_candidate_limit
            && plan.fused_pool_limit <= request.fused_pool_limit
            && plan.rerank_candidate_limit <= request.rerank_candidate_limit
            && plan.full_precision_rescore_limit <= request.full_precision_rescore_limit
            && plan.hydration_limit <= request.hydration_limit
            && plan.max_ssd_pages <= request.max_ssd_pages
            && plan.max_bytes_read <= request.max_bytes_read
            && plan.max_cpu_micros <= request.max_cpu_micros
            && plan.max_work_units <= request.max_work_units
            && plan.max_wall_time_micros <= request.max_wall_time_micros
            && plan.max_concurrent_stages <= request.max_concurrent_stages
            && plan.max_stage_attempts <= request.max_stage_attempts
            && plan.debug_record_limit <= request.debug_record_limit
    }

    /// Rejects a plan budget that widens any caller-provided hard cap.
    pub fn ensure_not_wider_than(&self, caller: &Self) -> SearchPlannerResult<()> {
        self.validate()?;
        caller.validate()?;
        if self.is_not_wider_than(caller) {
            Ok(())
        } else {
            Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::PlanBudgetWidened,
            ))
        }
    }
}
