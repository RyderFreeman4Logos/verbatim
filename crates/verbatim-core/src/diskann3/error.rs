//! Closed, diagnostic-only failures for the DiskANN3 retrieval contract.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Result alias for DiskANN3 contract operations.
pub type VectorSearchResult<T> = Result<T, VectorSearchError>;

/// Closed diagnostic taxonomy. No variant retains caller-controlled input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorSearchDiagnosticCode {
    BudgetExceeded,
    GenerationMismatch,
    FilterUnsupported,
    DimensionMismatch,
    ShardCorrupt,
    LegacyBackendOptInRequired,
    TombstonedVector,
    StageOutputExceeded,
    StageOrderInvalid,
    InvalidManifest,
    SerializationFailed,
    InvalidSearchBudget,
    InvalidCandidates,
}

impl VectorSearchDiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BudgetExceeded => "budget_exceeded",
            Self::GenerationMismatch => "generation_mismatch",
            Self::FilterUnsupported => "filter_unsupported",
            Self::DimensionMismatch => "dimension_mismatch",
            Self::ShardCorrupt => "shard_corrupt",
            Self::LegacyBackendOptInRequired => "legacy_backend_opt_in_required",
            Self::TombstonedVector => "tombstoned_vector",
            Self::StageOutputExceeded => "stage_output_exceeded",
            Self::StageOrderInvalid => "stage_order_invalid",
            Self::InvalidManifest => "invalid_manifest",
            Self::SerializationFailed => "serialization_failed",
            Self::InvalidSearchBudget => "invalid_search_budget",
            Self::InvalidCandidates => "invalid_candidates",
        }
    }
}

/// A retrieval contract failure containing only a closed diagnostic code.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum VectorSearchError {
    Contract { code: VectorSearchDiagnosticCode },
}

impl VectorSearchError {
    pub const fn contract(code: VectorSearchDiagnosticCode) -> Self {
        Self::Contract { code }
    }

    pub const fn diagnostic_code(self) -> VectorSearchDiagnosticCode {
        match self {
            Self::Contract { code } => code,
        }
    }
}

impl fmt::Debug for VectorSearchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "VectorSearchError({})",
            self.diagnostic_code().as_str()
        )
    }
}

impl fmt::Display for VectorSearchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "diskann3.{}", self.diagnostic_code().as_str())
    }
}

impl Error for VectorSearchError {}
