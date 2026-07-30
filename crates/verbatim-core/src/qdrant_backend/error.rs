//! Fail-closed, diagnostic-only errors for the Qdrant reference-backend contract.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Result alias for Qdrant backend-contract operations.
pub type QdrantBackendResult<T> = Result<T, QdrantBackendError>;

/// Closed diagnostic taxonomy for Qdrant adapter validation failures.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QdrantBackendDiagnosticCode {
    VectorDimensionMismatch,
    InvalidCollectionName,
    InvalidNamedVectorSpace,
    InvalidProfileId,
    InvalidGeneration,
    InvalidConfigDigest,
    InvalidSchema,
    InvalidPayloadIndexPlan,
    StrictFilterUnsupported,
    UnconditionalLocalPreSearchForbidden,
    FallbackWithoutTypedFailure,
    FallbackBudgetExhausted,
    StaleGenerationHydration,
    WrongGenerationHydration,
    InvalidCapabilities,
    LexicalConformanceRequired,
    InvalidSearchBudget,
    SearchBudgetWidened,
    InvalidSearchPolicy,
    InvalidFilterContract,
    InvalidHydrationRequest,
    InvalidGrpcPathRequirements,
    InvalidMutationHook,
    SerdeRevalidationFailed,
}

impl QdrantBackendDiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VectorDimensionMismatch => "vector_dimension_mismatch",
            Self::InvalidCollectionName => "invalid_collection_name",
            Self::InvalidNamedVectorSpace => "invalid_named_vector_space",
            Self::InvalidProfileId => "invalid_profile_id",
            Self::InvalidGeneration => "invalid_generation",
            Self::InvalidConfigDigest => "invalid_config_digest",
            Self::InvalidSchema => "invalid_schema",
            Self::InvalidPayloadIndexPlan => "invalid_payload_index_plan",
            Self::StrictFilterUnsupported => "strict_filter_unsupported",
            Self::UnconditionalLocalPreSearchForbidden => {
                "unconditional_local_pre_search_forbidden"
            }
            Self::FallbackWithoutTypedFailure => "fallback_without_typed_failure",
            Self::FallbackBudgetExhausted => "fallback_budget_exhausted",
            Self::StaleGenerationHydration => "stale_generation_hydration",
            Self::WrongGenerationHydration => "wrong_generation_hydration",
            Self::InvalidCapabilities => "invalid_capabilities",
            Self::LexicalConformanceRequired => "lexical_conformance_required",
            Self::InvalidSearchBudget => "invalid_search_budget",
            Self::SearchBudgetWidened => "search_budget_widened",
            Self::InvalidSearchPolicy => "invalid_search_policy",
            Self::InvalidFilterContract => "invalid_filter_contract",
            Self::InvalidHydrationRequest => "invalid_hydration_request",
            Self::InvalidGrpcPathRequirements => "invalid_grpc_path_requirements",
            Self::InvalidMutationHook => "invalid_mutation_hook",
            Self::SerdeRevalidationFailed => "serde_revalidation_failed",
        }
    }
}

/// A contract failure that retains only a stable diagnostic code.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(try_from = "QdrantBackendErrorSerde", into = "QdrantBackendErrorSerde")]
pub enum QdrantBackendError {
    Contract { code: QdrantBackendDiagnosticCode },
}

#[derive(Clone, Copy, Serialize, Deserialize)]
struct QdrantBackendErrorSerde {
    code: QdrantBackendDiagnosticCode,
}

impl From<QdrantBackendError> for QdrantBackendErrorSerde {
    fn from(value: QdrantBackendError) -> Self {
        Self {
            code: value.diagnostic_code(),
        }
    }
}

impl TryFrom<QdrantBackendErrorSerde> for QdrantBackendError {
    type Error = QdrantBackendError;

    fn try_from(value: QdrantBackendErrorSerde) -> Result<Self, Self::Error> {
        // Closed set: any deserialized code is already from the enum; re-validate by
        // reconstructing through the public constructor path.
        let reconstructed = QdrantBackendError::contract(value.code);
        if reconstructed.diagnostic_code().as_str().is_empty() {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::SerdeRevalidationFailed,
            ));
        }
        Ok(reconstructed)
    }
}

impl QdrantBackendError {
    pub const fn contract(code: QdrantBackendDiagnosticCode) -> Self {
        Self::Contract { code }
    }

    pub const fn diagnostic_code(self) -> QdrantBackendDiagnosticCode {
        match self {
            Self::Contract { code } => code,
        }
    }
}

impl fmt::Debug for QdrantBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "QdrantBackendError({})",
            self.diagnostic_code().as_str()
        )
    }
}

impl fmt::Display for QdrantBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "qdrant-backend.{}",
            self.diagnostic_code().as_str()
        )
    }
}

impl Error for QdrantBackendError {}
