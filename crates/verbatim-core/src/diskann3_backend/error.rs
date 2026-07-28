//! Fail-closed, diagnostic-only errors for the DiskANN3 adapter contract.

use std::error::Error;
use std::fmt;

/// Result alias for DiskANN3 backend-contract operations.
pub type DiskAnnBackendResult<T> = Result<T, DiskAnnBackendError>;

/// Closed diagnostic taxonomy for DiskANN3 adapter validation failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiskAnnBackendDiagnosticCode {
    VectorDimensionMismatch,
    NonFiniteVector,
    ZeroVector,
    MetricNormalizationMismatch,
    ProfileMismatch,
    GenerationMismatch,
    InvalidStableVectorId,
    InvalidMappingVersion,
    InvalidChunkIdMapping,
    DuplicateStableVectorId,
    VectorSpaceMismatch,
    InvalidSearchBudget,
    SearchBudgetWidened,
    InvalidGenerationContext,
    InvalidPredicatePlan,
    FullQualityViolation,
    InvalidCandidateScore,
    InvalidExactRescoreRequest,
    ExactRescoreIneligible,
    InvalidDistanceRange,
    InvalidSearchRequest,
    InvalidCapabilities,
    CapabilityBudgetExceeded,
    PageCacheDiagnosticsExceeded,
    InvalidIdempotencyKey,
    InvalidMutationBatch,
    DuplicateMutationVectorId,
    InvalidShardGenerationRequest,
    InvalidSnapshotId,
    ShutdownNotComplete,
}

impl DiskAnnBackendDiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VectorDimensionMismatch => "vector_dimension_mismatch",
            Self::NonFiniteVector => "non_finite_vector",
            Self::ZeroVector => "zero_vector",
            Self::MetricNormalizationMismatch => "metric_normalization_mismatch",
            Self::ProfileMismatch => "profile_mismatch",
            Self::GenerationMismatch => "generation_mismatch",
            Self::InvalidStableVectorId => "invalid_stable_vector_id",
            Self::InvalidMappingVersion => "invalid_mapping_version",
            Self::InvalidChunkIdMapping => "invalid_chunk_id_mapping",
            Self::DuplicateStableVectorId => "duplicate_stable_vector_id",
            Self::VectorSpaceMismatch => "vector_space_mismatch",
            Self::InvalidSearchBudget => "invalid_search_budget",
            Self::SearchBudgetWidened => "search_budget_widened",
            Self::InvalidGenerationContext => "invalid_generation_context",
            Self::InvalidPredicatePlan => "invalid_predicate_plan",
            Self::FullQualityViolation => "full_quality_violation",
            Self::InvalidCandidateScore => "invalid_candidate_score",
            Self::InvalidExactRescoreRequest => "invalid_exact_rescore_request",
            Self::ExactRescoreIneligible => "exact_rescore_ineligible",
            Self::InvalidDistanceRange => "invalid_distance_range",
            Self::InvalidSearchRequest => "invalid_search_request",
            Self::InvalidCapabilities => "invalid_capabilities",
            Self::CapabilityBudgetExceeded => "capability_budget_exceeded",
            Self::PageCacheDiagnosticsExceeded => "page_cache_diagnostics_exceeded",
            Self::InvalidIdempotencyKey => "invalid_idempotency_key",
            Self::InvalidMutationBatch => "invalid_mutation_batch",
            Self::DuplicateMutationVectorId => "duplicate_mutation_vector_id",
            Self::InvalidShardGenerationRequest => "invalid_shard_generation_request",
            Self::InvalidSnapshotId => "invalid_snapshot_id",
            Self::ShutdownNotComplete => "shutdown_not_complete",
        }
    }
}

/// A contract failure that retains only a stable diagnostic code.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DiskAnnBackendError {
    Contract { code: DiskAnnBackendDiagnosticCode },
}

impl DiskAnnBackendError {
    pub const fn contract(code: DiskAnnBackendDiagnosticCode) -> Self {
        Self::Contract { code }
    }

    pub const fn diagnostic_code(self) -> DiskAnnBackendDiagnosticCode {
        match self {
            Self::Contract { code } => code,
        }
    }
}

impl fmt::Debug for DiskAnnBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "DiskAnnBackendError({})",
            self.diagnostic_code().as_str()
        )
    }
}

impl fmt::Display for DiskAnnBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "diskann3-backend.{}",
            self.diagnostic_code().as_str()
        )
    }
}

impl Error for DiskAnnBackendError {}
