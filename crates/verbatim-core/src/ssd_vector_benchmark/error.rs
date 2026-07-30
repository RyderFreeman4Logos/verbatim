//! Closed, payload-free diagnostics for the SSD vector benchmark contract.
//!
//! Refs #382 / EVAL-SSD-001.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Result alias for SSD vector benchmark contract operations.
pub type SsdVectorBenchmarkResult<T> = Result<T, SsdVectorBenchmarkError>;

/// Closed diagnostic taxonomy for SSD vector benchmark validation failures.
///
/// Variants intentionally retain no query text, paths, identifiers, corpus
/// contents, backend values, or other caller-controlled payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SsdVectorBenchmarkDiagnosticCode {
    /// Embedding dimension is not the required full dimension (4096).
    DimensionReductionForbidden,
    /// Exact full-dimensional ground truth is missing or misconfigured.
    ExactGroundTruthRequired,
    /// Original vectors are not full-precision f32 for final scoring.
    OriginalVectorsMustBeFullPrecision,
    /// Candidate quality gates must remain separate from final rescore gates.
    CandidateFinalQualityMustBeSeparate,
    /// A required backend or role is missing from the systems catalog.
    MissingRequiredBackend,
    /// Backend role is invalid for the requested promotion or comparison use.
    InvalidBackendRole,
    /// Cold and/or warm cache states are missing from a required scenario set.
    MissingColdWarmCacheState,
    /// Cgroup memory accounting fields are missing or unknown.
    MissingCgroupMemoryMeasurement,
    /// Cross-backend digests for vectors/filters/budgets/qrels do not match.
    UnequalComparisonIdentity,
    /// Storage-growth series for N and/or source count is missing.
    MissingStorageGrowthSeries,
    /// A measurement required by a hard gate is absent (fail-closed).
    MissingMeasurement,
    /// A quality, resource, or latency threshold is not satisfied.
    GateThresholdNotMet,
    /// A closed identity label is empty, oversized, or malformed.
    InvalidIdentity,
    /// A numeric bound is zero, non-finite, or out of the contract range.
    InvalidBounds,
    /// A report or plan is missing a required field or component.
    MissingComponent,
    /// Hardware profile binding is incomplete for absolute latency evaluation.
    IncompleteHardwareProfile,
    /// Local-subset plan cannot be constructed under the closed rules.
    InvalidLocalSubsetPlan,
    /// Serde payload revalidation rejected an invariant.
    DeserializationRejected,
    /// Promotion was attempted from a regression-only backend alone.
    RegressionOnlyCannotPromote,
    /// A reference backend won the complete gate suite.
    ArchitectureDecisionMustBeReconsidered,
}

impl SsdVectorBenchmarkDiagnosticCode {
    /// Every closed code, for exhaustive contract tests and stable adapters.
    pub const ALL: [Self; 20] = [
        Self::DimensionReductionForbidden,
        Self::ExactGroundTruthRequired,
        Self::OriginalVectorsMustBeFullPrecision,
        Self::CandidateFinalQualityMustBeSeparate,
        Self::MissingRequiredBackend,
        Self::InvalidBackendRole,
        Self::MissingColdWarmCacheState,
        Self::MissingCgroupMemoryMeasurement,
        Self::UnequalComparisonIdentity,
        Self::MissingStorageGrowthSeries,
        Self::MissingMeasurement,
        Self::GateThresholdNotMet,
        Self::InvalidIdentity,
        Self::InvalidBounds,
        Self::MissingComponent,
        Self::IncompleteHardwareProfile,
        Self::InvalidLocalSubsetPlan,
        Self::DeserializationRejected,
        Self::RegressionOnlyCannotPromote,
        Self::ArchitectureDecisionMustBeReconsidered,
    ];

    /// Returns the stable machine-readable code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DimensionReductionForbidden => "dimension_reduction_forbidden",
            Self::ExactGroundTruthRequired => "exact_ground_truth_required",
            Self::OriginalVectorsMustBeFullPrecision => "original_vectors_must_be_full_precision",
            Self::CandidateFinalQualityMustBeSeparate => "candidate_final_quality_must_be_separate",
            Self::MissingRequiredBackend => "missing_required_backend",
            Self::InvalidBackendRole => "invalid_backend_role",
            Self::MissingColdWarmCacheState => "missing_cold_warm_cache_state",
            Self::MissingCgroupMemoryMeasurement => "missing_cgroup_memory_measurement",
            Self::UnequalComparisonIdentity => "unequal_comparison_identity",
            Self::MissingStorageGrowthSeries => "missing_storage_growth_series",
            Self::MissingMeasurement => "missing_measurement",
            Self::GateThresholdNotMet => "gate_threshold_not_met",
            Self::InvalidIdentity => "invalid_identity",
            Self::InvalidBounds => "invalid_bounds",
            Self::MissingComponent => "missing_component",
            Self::IncompleteHardwareProfile => "incomplete_hardware_profile",
            Self::InvalidLocalSubsetPlan => "invalid_local_subset_plan",
            Self::DeserializationRejected => "deserialization_rejected",
            Self::RegressionOnlyCannotPromote => "regression_only_cannot_promote",
            Self::ArchitectureDecisionMustBeReconsidered => {
                "architecture_decision_must_be_reconsidered"
            }
        }
    }
}

/// A fail-closed benchmark failure containing only a stable diagnostic code.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct SsdVectorBenchmarkError {
    code: SsdVectorBenchmarkDiagnosticCode,
}

impl SsdVectorBenchmarkError {
    /// Builds a diagnostic-only error without any caller-controlled data.
    pub const fn contract(code: SsdVectorBenchmarkDiagnosticCode) -> Self {
        Self { code }
    }

    /// Returns the closed diagnostic code.
    pub const fn diagnostic_code(self) -> SsdVectorBenchmarkDiagnosticCode {
        self.code
    }
}

impl fmt::Debug for SsdVectorBenchmarkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "SsdVectorBenchmarkError({})", self.code.as_str())
    }
}

impl fmt::Display for SsdVectorBenchmarkError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ssd-vector-benchmark.{}", self.code.as_str())
    }
}

impl Error for SsdVectorBenchmarkError {}
