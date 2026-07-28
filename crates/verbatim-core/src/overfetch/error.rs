//! Closed diagnostic-only failures for bounded retrieval orchestration.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Result alias for overfetch-elimination contract operations.
pub type OverfetchResult<T> = Result<T, OverfetchError>;

/// Closed retrieval-orchestration diagnostic taxonomy.
///
/// No variant retains a caller-controlled identifier, filter, SQL statement,
/// backend response, or secret. This keeps public `Debug` and `Display`
/// rendering safe to expose in operational diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OverfetchError {
    BudgetExceeded,
    CorpusSizeTopKForbidden,
    NPlusOneDetected,
    UnboundedHydration,
    UnsupportedStrictFilter,
    PrimaryBackendRequired,
}

impl OverfetchError {
    /// Every closed diagnostic code, useful for exhaustive contract tests.
    pub const ALL: [Self; 6] = [
        Self::BudgetExceeded,
        Self::CorpusSizeTopKForbidden,
        Self::NPlusOneDetected,
        Self::UnboundedHydration,
        Self::UnsupportedStrictFilter,
        Self::PrimaryBackendRequired,
    ];

    /// Stable machine-readable diagnostic code without caller-controlled data.
    pub const fn diagnostic_code(self) -> &'static str {
        match self {
            Self::BudgetExceeded => "budget_exceeded",
            Self::CorpusSizeTopKForbidden => "corpus_size_top_k_forbidden",
            Self::NPlusOneDetected => "n_plus_one_detected",
            Self::UnboundedHydration => "unbounded_hydration",
            Self::UnsupportedStrictFilter => "unsupported_strict_filter",
            Self::PrimaryBackendRequired => "primary_backend_required",
        }
    }
}

impl fmt::Display for OverfetchError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "overfetch.{}", self.diagnostic_code())
    }
}

impl Error for OverfetchError {}
