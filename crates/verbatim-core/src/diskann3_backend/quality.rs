//! Full-quality guarantees and exact-vector rescore boundary types.

use crate::diskann3::PublicationGeneration;

use super::{
    DiskAnnBackendDiagnosticCode, DiskAnnBackendError, DiskAnnBackendResult, GenerationContext,
    StableVectorId, VectorInput, VectorMetric, VectorSpaceSpec,
};

/// Representation allowed only for approximate candidate generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateRepresentation {
    /// Candidates use the authoritative original `f32` vector.
    OriginalF32,
    /// Candidates use product-quantized codes.
    ProductQuantized,
    /// Candidates use scalar-quantized values.
    ScalarQuantized,
    /// Candidates use a spherical/projection representation.
    Spherical,
}

/// Proof obligations required before a backend may claim full-quality results.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullQualityGuarantee {
    candidate_representation: CandidateRepresentation,
    original_vectors_on_ssd: bool,
    final_candidates_exact_rescoreable: bool,
}

impl FullQualityGuarantee {
    /// Rejects any capability claim that omits original-vector SSD access or exact rescoring.
    pub fn new(
        candidate_representation: CandidateRepresentation,
        original_vectors_on_ssd: bool,
        final_candidates_exact_rescoreable: bool,
    ) -> DiskAnnBackendResult<Self> {
        if !original_vectors_on_ssd || !final_candidates_exact_rescoreable {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::FullQualityViolation,
            ));
        }
        Ok(Self {
            candidate_representation,
            original_vectors_on_ssd,
            final_candidates_exact_rescoreable,
        })
    }

    /// Returns the candidate-only representation.
    pub const fn candidate_representation(&self) -> CandidateRepresentation {
        self.candidate_representation
    }

    /// Confirms original 4,096-dimensional `f32` vectors remain on SSD.
    pub const fn original_vectors_on_ssd(&self) -> bool {
        self.original_vectors_on_ssd
    }

    /// Confirms final candidates can be rescored from original vectors.
    pub const fn final_candidates_exact_rescoreable(&self) -> bool {
        self.final_candidates_exact_rescoreable
    }

    /// Returns the immutable original-vector dimensionality.
    pub const fn original_dimension(&self) -> usize {
        VectorSpaceSpec::DIMENSION
    }
}

/// Metric-labelled raw distance plus a backend-normalized score.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CandidateScore {
    metric: VectorMetric,
    raw_distance: f32,
    normalized_score: f32,
}

impl CandidateScore {
    /// Keeps raw metric distance separate from any metric-specific normalized score.
    pub fn new(
        metric: VectorMetric,
        raw_distance: f32,
        normalized_score: f32,
    ) -> DiskAnnBackendResult<Self> {
        if !raw_distance.is_finite() || !normalized_score.is_finite() {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidCandidateScore,
            ));
        }
        Ok(Self {
            metric,
            raw_distance,
            normalized_score,
        })
    }

    /// Returns the metric that gives meaning to `raw_distance`.
    pub const fn metric(&self) -> VectorMetric {
        self.metric
    }

    /// Returns the untransformed distance for this score's metric.
    pub const fn raw_distance(&self) -> f32 {
        self.raw_distance
    }

    /// Returns a score normalized only within the backend's declared metric policy.
    pub const fn normalized_score(&self) -> f32 {
        self.normalized_score
    }
}

/// Candidate identity plus proof that its original vector may be fetched for final rescoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExactRescoreCandidate {
    vector_id: StableVectorId,
    generation: PublicationGeneration,
    exact_rescore_eligible: bool,
}

impl ExactRescoreCandidate {
    /// Marks whether the backend can fetch this candidate's original SSD vector.
    pub const fn new(
        vector_id: StableVectorId,
        generation: PublicationGeneration,
        exact_rescore_eligible: bool,
    ) -> Self {
        Self {
            vector_id,
            generation,
            exact_rescore_eligible,
        }
    }

    /// Returns the stable identity for the candidate.
    pub const fn vector_id(&self) -> StableVectorId {
        self.vector_id
    }
}

/// Bounded exact-vector fetch request for the final rescore set.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExactVectorFetchRequest {
    context: GenerationContext,
    candidates: Vec<ExactRescoreCandidate>,
}

impl ExactVectorFetchRequest {
    /// Requires a nonempty, generation-consistent, exact-rescoreable final candidate set.
    pub fn new(
        context: GenerationContext,
        candidates: Vec<ExactRescoreCandidate>,
    ) -> DiskAnnBackendResult<Self> {
        let limit = context
            .budget_binding()
            .operation_budget()
            .fields()
            .full_precision_rescore_limit as usize;
        if candidates.is_empty() || candidates.len() > limit {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidExactRescoreRequest,
            ));
        }
        for candidate in &candidates {
            if candidate.generation != context.generation() {
                return Err(DiskAnnBackendError::contract(
                    DiskAnnBackendDiagnosticCode::GenerationMismatch,
                ));
            }
            if !candidate.exact_rescore_eligible {
                return Err(DiskAnnBackendError::contract(
                    DiskAnnBackendDiagnosticCode::ExactRescoreIneligible,
                ));
            }
        }
        Ok(Self {
            context,
            candidates,
        })
    }

    /// Returns the generation-scoped request context.
    pub const fn context(&self) -> &GenerationContext {
        &self.context
    }

    /// Returns only candidates eligible for exact original-vector fetches.
    pub fn candidates(&self) -> &[ExactRescoreCandidate] {
        &self.candidates
    }
}

/// Original full-precision vector returned for one stable candidate identity.
#[derive(Clone, PartialEq)]
pub struct ExactVector {
    vector_id: StableVectorId,
    input: VectorInput,
}

impl ExactVector {
    /// Revalidates the original vector before a provider exposes it for final scoring.
    pub fn new(
        context: &GenerationContext,
        vector_id: StableVectorId,
        input: VectorInput,
    ) -> DiskAnnBackendResult<Self> {
        context.validate_input(&input)?;
        Ok(Self { vector_id, input })
    }

    /// Returns the stable vector identity.
    pub const fn vector_id(&self) -> StableVectorId {
        self.vector_id
    }

    /// Returns the original full-precision components.
    pub fn values(&self) -> &[f32] {
        self.input.values()
    }
}

/// Exact original vectors fetched for final rescoring.
#[derive(Clone, PartialEq)]
pub struct ExactVectorFetchResponse {
    vectors: Vec<ExactVector>,
}

impl ExactVectorFetchResponse {
    /// Creates a response whose entries are individually validated by [`ExactVector::new`].
    pub fn new(vectors: Vec<ExactVector>) -> Self {
        Self { vectors }
    }

    /// Returns the fetched original vectors.
    pub fn vectors(&self) -> &[ExactVector] {
        &self.vectors
    }
}
