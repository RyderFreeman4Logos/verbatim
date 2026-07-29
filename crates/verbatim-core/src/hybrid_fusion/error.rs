//! Typed, diagnostic-only errors for hybrid-fusion contracts.
//!
//! No variant retains a caller-controlled identifier, document text, backend
//! response, filter expression, embedding, locator, or secret. The public
//! `Debug` and `Display` renderings are therefore safe to expose in operational
//! diagnostics and audit artifacts.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::{CompletenessState, FusionBudgetExhaustion, FusionStage, ScoreNormalizationKind};

/// Result alias for hybrid-fusion contract operations.
pub type FusionResult<T> = Result<T, FusionError>;

/// Result alias retained for callers that prefer the longer namespace.
pub type HybridFusionResult<T> = FusionResult<T>;

/// Closed fusion-orchestration diagnostic taxonomy.
///
/// Every code is a stable machine-readable string; none carries arbitrary
/// caller-controlled data. This keeps logs and audit artifacts redacted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FusionDiagnosticCode {
    // Retriever-result validation
    RetrieverIdEmpty,
    RetrieverResultRequiresCandidates,
    RetrieverResultDuplicateHitIds,
    RawRankMustBePositive,
    RawScoreNotFinite,
    FilterIdentityEmpty,
    RetrieverGenerationInvalid,
    CompletenessClaimUnsupportedForRetriever,
    // Fusion candidate validation
    FusionCandidateHitIdEmpty,
    FusionCandidateRequiresContributingRetriever,
    FusionCandidateProvenanceMissingRawRank,
    FusionCandidateDuplicateContributingRetriever,
    FusionCandidateInclusionReasonEmpty,
    // Profile validation
    ProfileVersionInvalid,
    ProfileWeightsMustBePositive,
    ProfileWeightsMustSumToUnit,
    ProfileCandidateLimitsMustBePositive,
    ProfileCandidateLimitsMustMonotonic,
    ProfileExplainabilityLevelInvalid,
    ProfileRrfConstantInvalid,
    ProfileSerializationFailed,
    ProfileHashMismatch,
    ProfileStrategyBackendOptInRequiresAcceptance,
    // Budget validation
    BudgetCapsMustBePositive,
    BudgetCapsMustMonotonic,
    // Stage / lifecycle
    IllegalStageTransition,
    FusionRequestRequiresRetrievers,
    // Completeness
    CompletenessScopeEmpty,
    CompletenessCoverageInvalid,
    CompletenessApproximateCannotClaimExhaustive,
    // Score normalization
    ScoreNormalizationUnsupportedForStrategy,
    NormalizedScoreNotFinite,
    // Output / codec
    InvalidStageOutputJson,
    StageOutputSerializationFailed,
    StageOutputRequiresRetrieverResults,
    StageOutputRequiresCandidates,
    StageOutputDuplicateCandidateHitId,
    StageOutputProvenanceRetrieverAbsent,
    StageOutputCandidateHitIdAbsentFromRetrievers,
    // Explainability
    ExplainabilityReportRequiresRows,
    ExplainabilityReportDuplicateRetriever,
}

/// Alias retained for callers that prefer the longer namespace.
pub type HybridFusionDiagnosticCode = FusionDiagnosticCode;

impl FusionDiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RetrieverIdEmpty => "retriever_id_empty",
            Self::RetrieverResultRequiresCandidates => "retriever_result_requires_candidates",
            Self::RetrieverResultDuplicateHitIds => "retriever_result_duplicate_hit_ids",
            Self::RawRankMustBePositive => "raw_rank_must_be_positive",
            Self::RawScoreNotFinite => "raw_score_not_finite",
            Self::FilterIdentityEmpty => "filter_identity_empty",
            Self::RetrieverGenerationInvalid => "retriever_generation_invalid",
            Self::CompletenessClaimUnsupportedForRetriever => {
                "completeness_claim_unsupported_for_retriever"
            }
            Self::FusionCandidateHitIdEmpty => "fusion_candidate_hit_id_empty",
            Self::FusionCandidateRequiresContributingRetriever => {
                "fusion_candidate_requires_contributing_retriever"
            }
            Self::FusionCandidateProvenanceMissingRawRank => {
                "fusion_candidate_provenance_missing_raw_rank"
            }
            Self::FusionCandidateDuplicateContributingRetriever => {
                "fusion_candidate_duplicate_contributing_retriever"
            }
            Self::FusionCandidateInclusionReasonEmpty => "fusion_candidate_inclusion_reason_empty",
            Self::ProfileVersionInvalid => "profile_version_invalid",
            Self::ProfileWeightsMustBePositive => "profile_weights_must_be_positive",
            Self::ProfileWeightsMustSumToUnit => "profile_weights_must_sum_to_unit",
            Self::ProfileCandidateLimitsMustBePositive => {
                "profile_candidate_limits_must_be_positive"
            }
            Self::ProfileCandidateLimitsMustMonotonic => "profile_candidate_limits_must_monotonic",
            Self::ProfileExplainabilityLevelInvalid => "profile_explainability_level_invalid",
            Self::ProfileRrfConstantInvalid => "profile_rrf_constant_invalid",
            Self::ProfileSerializationFailed => "profile_serialization_failed",
            Self::ProfileHashMismatch => "profile_hash_mismatch",
            Self::ProfileStrategyBackendOptInRequiresAcceptance => {
                "profile_strategy_backend_opt_in_requires_acceptance"
            }
            Self::BudgetCapsMustBePositive => "budget_caps_must_be_positive",
            Self::BudgetCapsMustMonotonic => "budget_caps_must_monotonic",
            Self::IllegalStageTransition => "illegal_stage_transition",
            Self::FusionRequestRequiresRetrievers => "fusion_request_requires_retrievers",
            Self::CompletenessScopeEmpty => "completeness_scope_empty",
            Self::CompletenessCoverageInvalid => "completeness_coverage_invalid",
            Self::CompletenessApproximateCannotClaimExhaustive => {
                "completeness_approximate_cannot_claim_exhaustive"
            }
            Self::ScoreNormalizationUnsupportedForStrategy => {
                "score_normalization_unsupported_for_strategy"
            }
            Self::NormalizedScoreNotFinite => "normalized_score_not_finite",
            Self::InvalidStageOutputJson => "invalid_stage_output_json",
            Self::StageOutputSerializationFailed => "stage_output_serialization_failed",
            Self::StageOutputRequiresRetrieverResults => "stage_output_requires_retriever_results",
            Self::StageOutputRequiresCandidates => "stage_output_requires_candidates",
            Self::StageOutputDuplicateCandidateHitId => "stage_output_duplicate_candidate_hit_id",
            Self::StageOutputProvenanceRetrieverAbsent => {
                "stage_output_provenance_retriever_absent"
            }
            Self::StageOutputCandidateHitIdAbsentFromRetrievers => {
                "stage_output_candidate_hit_id_absent_from_retrievers"
            }
            Self::ExplainabilityReportRequiresRows => "explainability_report_requires_rows",
            Self::ExplainabilityReportDuplicateRetriever => {
                "explainability_report_duplicate_retriever"
            }
        }
    }
}

/// Errors intentionally retain only closed diagnostic codes, never arbitrary
/// document text, embeddings, locators, credentials, or other secret-bearing
/// values. `Debug` is redacted to the diagnostic code string only.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum FusionError {
    Validation {
        code: FusionDiagnosticCode,
    },
    BudgetExhausted {
        exhaustion: FusionBudgetExhaustion,
    },
    IllegalTransition {
        from: FusionStage,
        to: FusionStage,
    },
    CompletenessViolation {
        state: CompletenessState,
        code: FusionDiagnosticCode,
    },
}

/// Alias retained for callers that prefer the longer namespace.
pub type HybridFusionError = FusionError;

impl FusionError {
    pub const fn validation(code: FusionDiagnosticCode) -> Self {
        Self::Validation { code }
    }

    pub const fn diagnostic_code(&self) -> FusionDiagnosticCode {
        match self {
            Self::Validation { code } | Self::CompletenessViolation { code, .. } => *code,
            Self::BudgetExhausted { .. } => FusionDiagnosticCode::BudgetCapsMustBePositive,
            Self::IllegalTransition { .. } => FusionDiagnosticCode::IllegalStageTransition,
        }
    }
}

impl fmt::Debug for FusionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redacted: emit only the stable diagnostic-code string. Never the
        // caller-controlled payload, stage identity beyond enum, or strategy.
        write!(f, "FusionError({})", self.diagnostic_code().as_str())
    }
}

impl fmt::Display for FusionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "hybrid-fusion.{}", self.diagnostic_code().as_str())
    }
}

impl Error for FusionError {}

// Re-export the ScoreNormalizationKind-using variant helper so callers can
// build normalized-score rejection errors without importing the kind enum.
impl FusionError {
    /// Builds a completeness-violation error carrying the offending state.
    pub fn completeness_violation(state: CompletenessState, code: FusionDiagnosticCode) -> Self {
        Self::CompletenessViolation { state, code }
    }

    /// Returns the strategy-normalization kind associated with this error, if any.
    pub fn strategy_normalization(&self) -> Option<ScoreNormalizationKind> {
        match self {
            Self::Validation {
                code: FusionDiagnosticCode::ScoreNormalizationUnsupportedForStrategy,
            } => Some(ScoreNormalizationKind::None),
            _ => None,
        }
    }
}
