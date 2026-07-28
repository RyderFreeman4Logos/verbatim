//! Closed, fail-closed diagnostic-only failures for exact filtered scans.

use std::error::Error;
use std::fmt;

/// Result alias for exact-scan contract operations.
pub type ExactScanResult<T> = Result<T, ExactScanError>;

/// Closed diagnostic taxonomy for exact-scan validation failures.
///
/// No variant retains a caller-controlled vector, identifier, filter, or
/// secret. This keeps public `Debug` and `Display` rendering safe to expose
/// in operational diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ExactScanDiagnosticCode {
    /// Vector does not have the required 4,096 dimensions.
    VectorDimensionMismatch,
    /// Vector contains one or more non-finite (NaN / Inf) components.
    NonFiniteVector,
    /// Vector is the zero vector and has no direction.
    ZeroVector,
    /// Vector does not satisfy the metric's normalization requirement.
    MetricNormalizationMismatch,
    /// Filter scope is empty, unsorted, contains duplicates, or is malformed.
    InvalidFilterScope,
    /// A budget limit (top-K, candidate cap, or I/O batch) was exceeded.
    BudgetExceeded,
    /// Two or more candidates share the same vector identifier.
    DuplicateCandidateId,
    /// Distance value is non-finite or violates metric range constraints.
    InvalidDistance,
    /// Top-K limit is zero.
    InvalidTopK,
    /// I/O batch size is zero.
    InvalidIoBatchSize,
    /// An exact or completeness claim was made without enumerating the scope.
    AuthorizedScopeNotEnumerated,
    /// A budget value is zero or structurally invalid.
    InvalidBudget,
    /// Duplicate vector identifier in a result set.
    DuplicateResultId,
    /// Result cardinality exceeds the declared top-K bound.
    ResultExceedsTopK,
    /// Ground-truth exhaustive scope is unbounded.
    GroundTruthScopeUnbounded,
    /// Candidate count in a rescore request exceeds the budget cap.
    CandidateCountExceedsCap,
}

impl ExactScanDiagnosticCode {
    /// Every closed diagnostic code, useful for exhaustive contract tests.
    pub const ALL: [Self; 16] = [
        Self::VectorDimensionMismatch,
        Self::NonFiniteVector,
        Self::ZeroVector,
        Self::MetricNormalizationMismatch,
        Self::InvalidFilterScope,
        Self::BudgetExceeded,
        Self::DuplicateCandidateId,
        Self::InvalidDistance,
        Self::InvalidTopK,
        Self::InvalidIoBatchSize,
        Self::AuthorizedScopeNotEnumerated,
        Self::InvalidBudget,
        Self::DuplicateResultId,
        Self::ResultExceedsTopK,
        Self::GroundTruthScopeUnbounded,
        Self::CandidateCountExceedsCap,
    ];

    /// Stable machine-readable diagnostic code without caller-controlled data.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VectorDimensionMismatch => "vector_dimension_mismatch",
            Self::NonFiniteVector => "non_finite_vector",
            Self::ZeroVector => "zero_vector",
            Self::MetricNormalizationMismatch => "metric_normalization_mismatch",
            Self::InvalidFilterScope => "invalid_filter_scope",
            Self::BudgetExceeded => "budget_exceeded",
            Self::DuplicateCandidateId => "duplicate_candidate_id",
            Self::InvalidDistance => "invalid_distance",
            Self::InvalidTopK => "invalid_top_k",
            Self::InvalidIoBatchSize => "invalid_io_batch_size",
            Self::AuthorizedScopeNotEnumerated => "authorized_scope_not_enumerated",
            Self::InvalidBudget => "invalid_budget",
            Self::DuplicateResultId => "duplicate_result_id",
            Self::ResultExceedsTopK => "result_exceeds_top_k",
            Self::GroundTruthScopeUnbounded => "ground_truth_scope_unbounded",
            Self::CandidateCountExceedsCap => "candidate_count_exceeds_cap",
        }
    }
}

/// A contract failure that retains only a stable diagnostic code.
///
/// No payload — the redacted `Debug` implementation renders only the code.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct ExactScanError {
    code: ExactScanDiagnosticCode,
}

impl ExactScanError {
    pub const fn contract(code: ExactScanDiagnosticCode) -> Self {
        Self { code }
    }

    pub const fn diagnostic_code(self) -> ExactScanDiagnosticCode {
        self.code
    }
}

impl fmt::Debug for ExactScanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ExactScanError({})", self.code.as_str())
    }
}

impl fmt::Display for ExactScanError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "exact-scan.{}", self.code.as_str())
    }
}

impl Error for ExactScanError {}
