//! Sealed retrieval-plan identity, path, and budget snapshot values.

use super::{
    FullPrecisionRescoreBudget, QuantizedCandidateGenerationProfile, SearchBudget,
    SearchPlannerDiagnosticCode, SearchPlannerError, SearchPlannerResult, SelectivityClass,
};

/// Immutable index generation to which a request and its result are bound.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct GenerationBinding(u64);

impl GenerationBinding {
    /// Creates a non-zero generation binding.
    pub fn new(value: u64) -> SearchPlannerResult<Self> {
        if value == 0 {
            Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::GenerationBindingInvalid,
            ))
        } else {
            Ok(Self(value))
        }
    }

    /// Returns the generation number.
    pub const fn value(&self) -> u64 {
        self.0
    }
}

/// Opaque deterministic identity for a sealed retrieval plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct PlanIdentity(u64);

impl PlanIdentity {
    pub(crate) fn from_snapshot(
        generation: GenerationBinding,
        path: SelectedPath,
        budget: SearchBudget,
        class: SelectivityClass,
    ) -> Self {
        let fields = budget.fields();
        let values = [
            generation.value(),
            path.discriminator(),
            class.discriminator(),
            u64::from(fields.result_limit),
            u64::from(fields.dense_candidate_limit),
            u64::from(fields.exact_candidate_limit),
            fields.max_work_units,
            fields.max_wall_time_micros,
        ];
        let mut value = 14_695_981_039_346_656_037_u64;
        for part in values {
            value ^= part;
            value = value.wrapping_mul(1_099_511_628_211_u64);
        }
        Self(if value == 0 { 1 } else { value })
    }

    /// Returns the opaque deterministic plan identity.
    pub const fn value(&self) -> u64 {
        self.0
    }
}

/// Named fallback profile used only when ordinary planning cannot safely proceed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum NamedDegradedProfile {
    /// Cardinality freshness, confidence, or correctness is insufficient.
    UntrustedCardinality,
    /// A strict predicate cannot be enforced at the required stage.
    UnsupportedStrictPredicate,
}

/// Candidate-generation path selected by the planner.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelectedPath {
    /// Exact sequential SIMD scan over the authorized subset.
    ExactSimdScan,
    /// Predicate-aware DiskANN3-style approximate candidate generation.
    PredicateAwareDiskAnn3,
    /// Explicit bounded enumeration for an exhaustive request.
    ExhaustiveEnumeration,
    /// A named, visible, hard-bounded degraded profile.
    NamedDegradedProfile(NamedDegradedProfile),
}

impl SelectedPath {
    pub(crate) const fn discriminator(self) -> u64 {
        match self {
            Self::ExactSimdScan => 1,
            Self::PredicateAwareDiskAnn3 => 2,
            Self::ExhaustiveEnumeration => 3,
            Self::NamedDegradedProfile(NamedDegradedProfile::UntrustedCardinality) => 4,
            Self::NamedDegradedProfile(NamedDegradedProfile::UnsupportedStrictPredicate) => 5,
        }
    }
}

/// Completeness expectation exposed before any adapter executes a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PlannedCompleteness {
    /// Exact result if execution completes within the sealed budget.
    ExactWithinAuthorizedScope,
    /// Approximate candidate generation is visible as partial by design.
    ApproximatePartial,
    /// Exhaustive coverage is possible only within the declared authorized scope and budget.
    ExhaustiveWithinAuthorizedScope,
    /// A named degraded profile is always partial.
    DegradedPartial,
}

impl PlannedCompleteness {
    pub(crate) const fn is_partial(self) -> bool {
        matches!(self, Self::ApproximatePartial | Self::DegradedPartial)
    }
}

/// Validated plan that cannot be constructed by callers outside the planner gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrievalPlan {
    identity: PlanIdentity,
    generation: GenerationBinding,
    path: SelectedPath,
    budget: SearchBudget,
    planned_completeness: PlannedCompleteness,
    approximate_profile: Option<QuantizedCandidateGenerationProfile>,
    full_precision_rescore_budget: FullPrecisionRescoreBudget,
}

impl RetrievalPlan {
    #[allow(clippy::too_many_arguments)]
    pub(crate) fn new(
        identity: PlanIdentity,
        generation: GenerationBinding,
        path: SelectedPath,
        budget: SearchBudget,
        caller_budget: SearchBudget,
        planned_completeness: PlannedCompleteness,
        approximate_profile: Option<QuantizedCandidateGenerationProfile>,
        full_precision_rescore_budget: FullPrecisionRescoreBudget,
    ) -> SearchPlannerResult<Self> {
        budget.ensure_not_wider_than(&caller_budget)?;
        let fields = budget.fields();
        let requires_approximation = path == SelectedPath::PredicateAwareDiskAnn3;
        let path_completeness_is_valid = matches!(
            (path, planned_completeness),
            (
                SelectedPath::PredicateAwareDiskAnn3,
                PlannedCompleteness::ApproximatePartial
            ) | (
                SelectedPath::ExactSimdScan,
                PlannedCompleteness::ExactWithinAuthorizedScope
            ) | (
                SelectedPath::ExhaustiveEnumeration,
                PlannedCompleteness::ExhaustiveWithinAuthorizedScope
            ) | (
                SelectedPath::NamedDegradedProfile(_),
                PlannedCompleteness::DegradedPartial
            )
        );
        if !path_completeness_is_valid
            || requires_approximation != approximate_profile.is_some()
            || full_precision_rescore_budget.candidate_limit() > fields.full_precision_rescore_limit
            || full_precision_rescore_budget.cpu_micros() > fields.max_cpu_micros
        {
            return Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::InvalidPlan,
            ));
        }
        if let Some(profile) = approximate_profile {
            if profile.candidate_limit() > fields.dense_candidate_limit {
                return Err(SearchPlannerError::new(
                    SearchPlannerDiagnosticCode::InvalidPlan,
                ));
            }
        }
        Ok(Self {
            identity,
            generation,
            path,
            budget,
            planned_completeness,
            approximate_profile,
            full_precision_rescore_budget,
        })
    }

    /// Returns the opaque plan identity carried into public records.
    pub const fn identity(&self) -> PlanIdentity {
        self.identity
    }

    /// Returns the immutable generation binding.
    pub const fn generation(&self) -> GenerationBinding {
        self.generation
    }

    /// Returns the selected retrieval path.
    pub const fn path(&self) -> SelectedPath {
        self.path
    }

    /// Returns the sealed non-widening budget snapshot.
    pub const fn budget(&self) -> SearchBudget {
        self.budget
    }

    /// Returns the plan's visible completeness expectation.
    pub const fn planned_completeness(&self) -> PlannedCompleteness {
        self.planned_completeness
    }

    /// Returns approximate-generation parameters only for the approximate path.
    pub const fn approximate_profile(&self) -> Option<QuantizedCandidateGenerationProfile> {
        self.approximate_profile
    }

    /// Returns the separate full-precision rescoring allocation.
    pub const fn full_precision_rescore_budget(&self) -> FullPrecisionRescoreBudget {
        self.full_precision_rescore_budget
    }
}
