//! Selectivity-aware path selection behind the atomic planner gate.

use super::cardinality::EstimateDisposition;
use super::{
    CandidateQuantizer, FullPrecisionRescoreBudget, NamedDegradedProfile, PlanIdentity,
    PlannedCompleteness, PlannerRequest, QuantizedCandidateGenerationProfile, RetrievalIntent,
    RetrievalPlan, SearchBudget, SearchPlannerDiagnosticCode, SearchPlannerError,
    SearchPlannerResult, SelectedPath, SelectivityClass, StrictPredicateHandlingMode,
};

/// Contract implemented by a planner that seals every public retrieval plan.
pub trait SearchPlannerContract {
    /// Validates all input and returns a plan that never widens the caller budget.
    fn plan_and_validate(&self, request: PlannerRequest) -> SearchPlannerResult<RetrievalPlan>;
}

/// Stateless reference implementation of the bounded retrieval-planning contract.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct SearchPlanner;

impl SearchPlanner {
    fn strict_failure(
        request: &PlannerRequest,
    ) -> SearchPlannerResult<(SelectedPath, PlannedCompleteness)> {
        match request.strict_predicate_handling {
            StrictPredicateHandlingMode::FailClosed => Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::StrictPredicateUnsupported,
            )),
            StrictPredicateHandlingMode::NamedBoundedDegradedProfile => Ok((
                SelectedPath::NamedDegradedProfile(
                    NamedDegradedProfile::UnsupportedStrictPredicate,
                ),
                PlannedCompleteness::DegradedPartial,
            )),
        }
    }

    fn select_exact(
        request: &PlannerRequest,
        matching_count: u64,
    ) -> SearchPlannerResult<(SelectedPath, PlannedCompleteness)> {
        if matching_count > u64::from(request.budget.fields().exact_candidate_limit) {
            return Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::BudgetExceeded,
            ));
        }
        if !request.capability.supports_exact_simd_scan() {
            return Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::CapabilityUnsupported,
            ));
        }
        if request.strict_predicate_required
            && !request.capability.supports_strict_pre_rank_predicates()
        {
            return Self::strict_failure(request);
        }
        Ok((
            SelectedPath::ExactSimdScan,
            PlannedCompleteness::ExactWithinAuthorizedScope,
        ))
    }

    fn select_exhaustive(
        request: &PlannerRequest,
        matching_count: u64,
    ) -> SearchPlannerResult<(SelectedPath, PlannedCompleteness)> {
        let maximum = request
            .selectivity
            .crossover()
            .exhaustive_enumeration_max_matches()
            .min(u64::from(request.budget.fields().exact_candidate_limit));
        if matching_count > maximum {
            return Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::ExhaustiveBudgetExceeded,
            ));
        }
        if !request.capability.supports_exhaustive_enumeration() {
            return Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::CapabilityUnsupported,
            ));
        }
        if request.strict_predicate_required
            && !request.capability.supports_strict_pre_rank_predicates()
        {
            return Self::strict_failure(request);
        }
        Ok((
            SelectedPath::ExhaustiveEnumeration,
            PlannedCompleteness::ExhaustiveWithinAuthorizedScope,
        ))
    }

    fn select_path(
        &self,
        request: &PlannerRequest,
    ) -> SearchPlannerResult<(SelectedPath, PlannedCompleteness)> {
        if request.selectivity.crossover().calibration_generation() != request.generation.value() {
            return Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::GenerationBindingInvalid,
            ));
        }
        request.capability.validate_budget(&request.budget)?;
        match request.cardinality.disposition(request.estimate_handling)? {
            EstimateDisposition::Degraded => {
                if request.strict_predicate_required
                    && !request.capability.supports_strict_pre_rank_predicates()
                {
                    return Self::strict_failure(request);
                }
                return Ok((
                    SelectedPath::NamedDegradedProfile(NamedDegradedProfile::UntrustedCardinality),
                    PlannedCompleteness::DegradedPartial,
                ));
            }
            EstimateDisposition::Trusted => {}
        }

        let matching_count = request.cardinality.matching_count();
        if request.selectivity.class() == SelectivityClass::SingleDocument && matching_count > 1 {
            return Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::InvalidPlan,
            ));
        }
        if request.intent == RetrievalIntent::Exhaustive
            || request.selectivity.requires_exhaustive_enumeration()
        {
            return Self::select_exhaustive(request, matching_count);
        }

        let crossover = request.selectivity.crossover();
        if matching_count <= crossover.exact_simd_scan_max_matches()
            || matching_count < crossover.predicate_aware_diskann3_min_matches()
        {
            return Self::select_exact(request, matching_count);
        }
        if request.strict_predicate_required
            && !request.capability.supports_predicate_aware_diskann3()
        {
            return Self::strict_failure(request);
        }
        if !request.capability.supports_predicate_aware_diskann3() {
            return Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::CapabilityUnsupported,
            ));
        }
        Ok((
            SelectedPath::PredicateAwareDiskAnn3,
            PlannedCompleteness::ApproximatePartial,
        ))
    }

    fn approximate_profile(
        budget: SearchBudget,
    ) -> SearchPlannerResult<QuantizedCandidateGenerationProfile> {
        let fields = budget.fields();
        QuantizedCandidateGenerationProfile::new(
            CandidateQuantizer::ProductQuantized,
            fields.dense_candidate_limit.min(fields.fused_pool_limit),
            fields
                .graph_candidate_limit
                .min(fields.dense_candidate_limit),
            fields
                .lexical_candidate_limit
                .min(fields.dense_candidate_limit),
        )
    }
}

impl SearchPlannerContract for SearchPlanner {
    fn plan_and_validate(&self, request: PlannerRequest) -> SearchPlannerResult<RetrievalPlan> {
        request.validate()?;
        let (path, planned_completeness) = self.select_path(&request)?;
        let approximate_profile = if path == SelectedPath::PredicateAwareDiskAnn3 {
            Some(Self::approximate_profile(request.budget)?)
        } else {
            None
        };
        let fields = request.budget.fields();
        let full_precision_rescore_budget = FullPrecisionRescoreBudget::new(
            fields.full_precision_rescore_limit,
            fields.max_cpu_micros,
        )?;
        let identity = PlanIdentity::from_snapshot(
            request.generation,
            path,
            request.budget,
            request.selectivity.class(),
        );
        RetrievalPlan::new(
            identity,
            request.generation,
            path,
            request.budget,
            request.budget,
            planned_completeness,
            approximate_profile,
            full_precision_rescore_budget,
        )
    }
}
