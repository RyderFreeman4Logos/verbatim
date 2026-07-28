//! Predicate-aware Top-K and range-search request/response boundary types.

use crate::diskann3::PublicationGeneration;

use super::{
    CandidateScore, DiskAnnBackendDiagnosticCode, DiskAnnBackendError, DiskAnnBackendResult,
    SearchContext, StableVectorId, VectorInput,
};

/// A finite inclusive range over metric-labelled raw distances.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct RawDistanceRange {
    minimum: f32,
    maximum: f32,
}

impl RawDistanceRange {
    /// Creates a finite, ordered raw-distance interval without assuming metric comparability.
    pub fn new(minimum: f32, maximum: f32) -> DiskAnnBackendResult<Self> {
        if !minimum.is_finite() || !maximum.is_finite() || minimum > maximum {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidDistanceRange,
            ));
        }
        Ok(Self { minimum, maximum })
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

/// One generation-bound approximate candidate and its explicitly metric-labelled score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct SearchCandidate {
    vector_id: StableVectorId,
    generation: PublicationGeneration,
    score: CandidateScore,
}

impl SearchCandidate {
    /// Creates a candidate whose score cannot be interpreted without its metric label.
    pub const fn new(
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

/// Bounded candidate page from either Top-K or range search.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchPage {
    candidates: Vec<SearchCandidate>,
}

impl SearchPage {
    /// Validates generation consistency and result cardinality before returning a candidate page.
    pub fn new(
        context: &SearchContext,
        candidates: Vec<SearchCandidate>,
    ) -> DiskAnnBackendResult<Self> {
        let limit = context
            .generation()
            .budget_binding()
            .operation_budget()
            .fields()
            .result_limit as usize;
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
        Ok(Self { candidates })
    }

    /// Returns the bounded candidate page.
    pub fn candidates(&self) -> &[SearchCandidate] {
        &self.candidates
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
