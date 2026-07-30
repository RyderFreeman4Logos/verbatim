//! Redacted fail-closed diagnostics for the LanceDB reference contract.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

pub type LanceDbBackendResult<T> = Result<T, LanceDbBackendError>;

/// Closed diagnostic taxonomy; details remain out of public errors and logs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanceDbBackendDiagnosticCode {
    VectorDimensionMismatch,
    InvalidTableName,
    InvalidProfileId,
    InvalidGeneration,
    InvalidConfigDigest,
    InvalidSchema,
    InvalidIndexProfile,
    InvalidScalarIndexPlan,
    InvalidFilterContract,
    StrictFilterUnbound,
    InvalidProbePlan,
    InvalidQualityPlan,
    InvalidCandidateLossReport,
    StaleGenerationHydration,
    WrongGenerationHydration,
    InvalidLifecycleTransition,
    InvalidCapabilities,
    LexicalConformanceRequired,
    InvalidSearchBudget,
    SearchBudgetWidened,
    InvalidSearchPolicy,
    GenerationMismatch,
}

impl LanceDbBackendDiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VectorDimensionMismatch => "vector_dimension_mismatch",
            Self::InvalidTableName => "invalid_table_name",
            Self::InvalidProfileId => "invalid_profile_id",
            Self::InvalidGeneration => "invalid_generation",
            Self::InvalidConfigDigest => "invalid_config_digest",
            Self::InvalidSchema => "invalid_schema",
            Self::InvalidIndexProfile => "invalid_index_profile",
            Self::InvalidScalarIndexPlan => "invalid_scalar_index_plan",
            Self::InvalidFilterContract => "invalid_filter_contract",
            Self::StrictFilterUnbound => "strict_filter_unbound",
            Self::InvalidProbePlan => "invalid_probe_plan",
            Self::InvalidQualityPlan => "invalid_quality_plan",
            Self::InvalidCandidateLossReport => "invalid_candidate_loss_report",
            Self::StaleGenerationHydration => "stale_generation_hydration",
            Self::WrongGenerationHydration => "wrong_generation_hydration",
            Self::InvalidLifecycleTransition => "invalid_lifecycle_transition",
            Self::InvalidCapabilities => "invalid_capabilities",
            Self::LexicalConformanceRequired => "lexical_conformance_required",
            Self::InvalidSearchBudget => "invalid_search_budget",
            Self::SearchBudgetWidened => "search_budget_widened",
            Self::InvalidSearchPolicy => "invalid_search_policy",
            Self::GenerationMismatch => "generation_mismatch",
        }
    }
}

/// Error carrying only its stable diagnostic code.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LanceDbBackendError(LanceDbBackendDiagnosticCode);

impl LanceDbBackendError {
    pub const fn contract(code: LanceDbBackendDiagnosticCode) -> Self {
        Self(code)
    }

    pub const fn diagnostic_code(self) -> LanceDbBackendDiagnosticCode {
        self.0
    }
}

impl fmt::Debug for LanceDbBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "LanceDbBackendError({})", self.0.as_str())
    }
}

impl fmt::Display for LanceDbBackendError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "lancedb-backend.{}", self.0.as_str())
    }
}

impl Error for LanceDbBackendError {}
