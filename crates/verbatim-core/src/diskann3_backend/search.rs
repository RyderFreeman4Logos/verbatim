//! Predicate-aware Top-K and range-search request/response boundary types.

use crate::diskann3::PublicationGeneration;

use super::{
    CandidateScore, DiskAnnBackendDiagnosticCode, DiskAnnBackendError, DiskAnnBackendResult,
    ExactRescoreCandidate, GenerationContext, SearchContext, StableVectorId, VectorInput,
    VectorMetric,
};

/// A finite inclusive range over metric-labelled raw distances.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawDistanceRange {
    metric: VectorMetric,
    minimum: f32,
    maximum: f32,
}

impl RawDistanceRange {
    /// Creates a finite, ordered raw-distance interval bound to one metric domain.
    pub fn new(metric: VectorMetric, minimum: f32, maximum: f32) -> DiskAnnBackendResult<Self> {
        if !minimum.is_finite()
            || !maximum.is_finite()
            || minimum > maximum
            || (metric == VectorMetric::L2 && minimum < 0.0)
        {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidDistanceRange,
            ));
        }
        Ok(Self {
            metric,
            minimum,
            maximum,
        })
    }

    /// Returns the metric that gives meaning to this raw-distance interval.
    pub const fn metric(&self) -> VectorMetric {
        self.metric
    }

    /// Returns the inclusive lower bound.
    pub const fn minimum(&self) -> f32 {
        self.minimum
    }

    /// Returns the inclusive upper bound.
    pub const fn maximum(&self) -> f32 {
        self.maximum
    }
}

/// Validated predicate-aware Top-K request.
#[derive(Clone, PartialEq)]
pub struct TopKSearchRequest {
    context: SearchContext,
    query: VectorInput,
    limit: usize,
}

impl TopKSearchRequest {
    /// Binds the query and result limit to one predicate-aware generation context.
    pub fn new(
        context: SearchContext,
        query: VectorInput,
        limit: usize,
    ) -> DiskAnnBackendResult<Self> {
        validate_result_limit(&context, limit)?;
        context.generation().validate_input(&query)?;
        Ok(Self {
            context,
            query,
            limit,
        })
    }

    /// Returns the predicate-aware context.
    pub const fn context(&self) -> &SearchContext {
        &self.context
    }

    /// Returns the validated query vector.
    pub const fn query(&self) -> &VectorInput {
        &self.query
    }

    /// Returns the caller-bounded result count.
    pub const fn limit(&self) -> usize {
        self.limit
    }
}

/// Validated predicate-aware raw-distance range-search request.
#[derive(Clone, PartialEq)]
pub struct RangeSearchRequest {
    context: SearchContext,
    query: VectorInput,
    range: RawDistanceRange,
    limit: usize,
}

impl RangeSearchRequest {
    /// Binds a finite raw-distance range to one predicate-aware generation context.
    pub fn new(
        context: SearchContext,
        query: VectorInput,
        range: RawDistanceRange,
        limit: usize,
    ) -> DiskAnnBackendResult<Self> {
        validate_result_limit(&context, limit)?;
        context.generation().validate_input(&query)?;
        if range.metric() != context.generation().vector_space().metric() {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidDistanceRange,
            ));
        }
        Ok(Self {
            context,
            query,
            range,
            limit,
        })
    }

    /// Returns the predicate-aware context.
    pub const fn context(&self) -> &SearchContext {
        &self.context
    }

    /// Returns the validated query vector.
    pub const fn query(&self) -> &VectorInput {
        &self.query
    }

    /// Returns the metric-labelled raw-distance interval.
    pub const fn range(&self) -> RawDistanceRange {
        self.range
    }

    /// Returns the caller-bounded result count.
    pub const fn limit(&self) -> usize {
        self.limit
    }
}

/// Opaque provider-issued approximate candidate with an explicitly metric-labelled score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchCandidate {
    vector_id: StableVectorId,
    generation: PublicationGeneration,
    score: CandidateScore,
}

impl SearchCandidate {
    /// Issues a candidate only after the adapter receives a provider search result.
    #[allow(dead_code)]
    pub(crate) const fn new(
        vector_id: StableVectorId,
        generation: PublicationGeneration,
        score: CandidateScore,
    ) -> Self {
        Self {
            vector_id,
            generation,
            score,
        }
    }

    /// Returns the stable candidate identity.
    pub const fn vector_id(&self) -> StableVectorId {
        self.vector_id
    }

    /// Returns the metric-labelled score.
    pub const fn score(&self) -> CandidateScore {
        self.score
    }
}

/// Opaque bounded candidate page issued by either Top-K or range search.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchPage {
    context: GenerationContext,
    candidates: Vec<SearchCandidate>,
}

impl SearchPage {
    /// Issues a page only after the adapter completes a concrete Top-K provider search.
    #[allow(dead_code)]
    pub(crate) fn from_top_k_request(
        request: &TopKSearchRequest,
        candidates: Vec<SearchCandidate>,
    ) -> DiskAnnBackendResult<Self> {
        Self::from_request(request.context(), request.limit(), candidates)
    }

    /// Issues a page only after the adapter completes a concrete range provider search.
    #[allow(dead_code)]
    pub(crate) fn from_range_search_request(
        request: &RangeSearchRequest,
        candidates: Vec<SearchCandidate>,
    ) -> DiskAnnBackendResult<Self> {
        Self::from_request(request.context(), request.limit(), candidates)
    }

    #[allow(dead_code)]
    fn from_request(
        context: &SearchContext,
        limit: usize,
        candidates: Vec<SearchCandidate>,
    ) -> DiskAnnBackendResult<Self> {
        if candidates.len() > limit {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidSearchRequest,
            ));
        }
        if candidates
            .iter()
            .any(|candidate| candidate.generation != context.generation().generation())
        {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::GenerationMismatch,
            ));
        }
        if candidates.iter().any(|candidate| {
            candidate.score.metric() != context.generation().vector_space().metric()
        }) {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidCandidateScore,
            ));
        }
        Ok(Self {
            context: context.generation().clone(),
            candidates,
        })
    }

    /// Returns the generation context that issued this page.
    pub const fn context(&self) -> &GenerationContext {
        &self.context
    }

    /// Returns the bounded candidate page.
    pub fn candidates(&self) -> &[SearchCandidate] {
        &self.candidates
    }

    /// Issues exact-rescore candidates only from this validated final candidate page.
    pub fn exact_rescore_candidates(&self) -> Vec<ExactRescoreCandidate> {
        self.candidates
            .iter()
            .map(|candidate| {
                ExactRescoreCandidate::from_search_page(
                    candidate.vector_id,
                    self.context.generation(),
                )
            })
            .collect()
    }
}

fn validate_result_limit(context: &SearchContext, limit: usize) -> DiskAnnBackendResult<()> {
    let caller_limit = context
        .generation()
        .budget_binding()
        .operation_budget()
        .fields()
        .result_limit as usize;
    if limit == 0 || limit > caller_limit {
        return Err(DiskAnnBackendError::contract(
            DiskAnnBackendDiagnosticCode::InvalidSearchRequest,
        ));
    }
    Ok(())
}
