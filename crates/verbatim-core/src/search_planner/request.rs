//! Authorized planner request values accepted by the atomic planning gate.

use super::{
    BackendCapability, CardinalityEstimate, EstimateHandlingMode, GenerationBinding, SearchBudget,
    SearchPlannerDiagnosticCode, SearchPlannerError, SearchPlannerResult, SelectivityProfile,
    VectorMetric,
};

/// Requested retrieval semantics independent of backend implementation details.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RetrievalIntent {
    /// Return the caller-bounded top-k records.
    TopK,
    /// Explicitly enumerate a bounded authorized subset.
    Exhaustive,
}

/// Required behavior when a strict predicate cannot be enforced safely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum StrictPredicateHandlingMode {
    /// Reject rather than evaluate the predicate at an unsafe stage.
    FailClosed,
    /// Return only a named, hard-bounded degraded profile.
    NamedBoundedDegradedProfile,
}

/// Verified precondition required by backends that cannot safely serve a cold cache.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WarmCacheAttestation {
    /// The authorization-bound backend adapter verified the cache is warm.
    Verified,
}

/// Fully authorization-bound input accepted by the planner's single validation gate.
///
/// This type deliberately contains no query payload, identifier, tenant value, or
/// authorization material. Adapters resolve those details before constructing it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlannerRequest {
    pub(crate) budget: SearchBudget,
    pub(crate) cardinality: CardinalityEstimate,
    pub(crate) selectivity: SelectivityProfile,
    pub(crate) capability: BackendCapability,
    pub(crate) warm_cache_attestation: Option<WarmCacheAttestation>,
    pub(crate) generation: GenerationBinding,
    pub(crate) intent: RetrievalIntent,
    pub(crate) estimate_handling: EstimateHandlingMode,
    pub(crate) strict_predicate_required: bool,
    pub(crate) strict_predicate_handling: StrictPredicateHandlingMode,
    pub(crate) vector_dimension: u32,
    pub(crate) vector_metric: VectorMetric,
}

impl PlannerRequest {
    /// Creates an authorization-bound request after basic structural validation.
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        budget: SearchBudget,
        cardinality: CardinalityEstimate,
        selectivity: SelectivityProfile,
        capability: BackendCapability,
        warm_cache_attestation: Option<WarmCacheAttestation>,
        generation: GenerationBinding,
        intent: RetrievalIntent,
        estimate_handling: EstimateHandlingMode,
        strict_predicate_required: bool,
        strict_predicate_handling: StrictPredicateHandlingMode,
        vector_dimension: u32,
        vector_metric: VectorMetric,
    ) -> SearchPlannerResult<Self> {
        let request = Self {
            budget,
            cardinality,
            selectivity,
            capability,
            warm_cache_attestation,
            generation,
            intent,
            estimate_handling,
            strict_predicate_required,
            strict_predicate_handling,
            vector_dimension,
            vector_metric,
        };
        request.validate()?;
        Ok(request)
    }

    pub(crate) fn validate(&self) -> SearchPlannerResult<()> {
        self.budget.validate()?;
        self.selectivity.validate()?;
        self.capability.validate()?;
        if self.capability.requires_verified_warm_cache_attestation()
            && self.warm_cache_attestation.is_none()
        {
            return Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::CapabilityUnsupported,
            ));
        }
        if self.vector_dimension == 0
            || !self.capability.supports_dimension(self.vector_dimension)
            || !self.capability.supports_metric(self.vector_metric)
            || !self.capability.binds_generation()
            || !self.capability.reports_required_work()
        {
            Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::CapabilityUnsupported,
            ))
        } else {
            Ok(())
        }
    }
}
