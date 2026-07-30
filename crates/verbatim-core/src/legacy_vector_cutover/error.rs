//! Closed diagnostics for legacy vector serving retirement (Refs #388).

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Result alias for legacy vector cutover contract operations.
pub type LegacyRetirementResult<T> = Result<T, LegacyRetirementError>;

/// Closed diagnostic taxonomy for a fail-closed legacy serving retirement.
///
/// No variant retains caller-controlled identifiers, paths, vector bytes,
/// profile values, query data, or operational measurements.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LegacyRetirementDiagnosticCode {
    /// DiskANN3 compiled but no substantive cutover evidence was provided.
    DiskAnn3CompileOnly,
    /// VectorSearch conformance evidence is absent.
    MissingVectorSearchConformance,
    /// Metric, profile, or source-generation validation is absent.
    MissingExactIdentityValidation,
    /// Authorized-subset exact recall evidence is absent.
    MissingAuthorizedSubsetRecall,
    /// Cgroup memory or SSD-I/O evidence is absent.
    MissingResourceGate,
    /// Cold/warm latency or concurrency evidence is absent.
    MissingLatencyConcurrencyGate,
    /// Update/delete/compaction/recovery evidence is absent.
    MissingMutationRecoveryGate,
    /// Staged dual-generation shadow evidence is absent.
    MissingDualGenerationShadow,
    /// Rollback or disaster-recovery exercise evidence is absent.
    MissingRollbackDisasterRecovery,
    /// Operator documentation or migration-tooling readiness is absent.
    MissingOperatorReadiness,
    /// A required count/hash/dimension/metric/normalization/ID/filter/recall
    /// validation artifact is absent.
    MigrationValidationIncomplete,
    /// The source and target dimensions differ.
    DimensionReductionForbidden,
    /// Authoritative vector profile metadata or bytes cannot be reused and a
    /// caller attempted to continue without an explicit re-embedding workflow.
    SilentReembeddingForbidden,
    /// Shadow comparison has not completed.
    ShadowComparisonIncomplete,
    /// Shadow comparison completed but did not pass.
    ShadowComparisonFailed,
    /// Promotion did not bind to the exact candidate shadow generation.
    PromotionGenerationMismatch,
    /// A shadow comparison attempted to compare a generation with itself.
    ShadowComparisonSameGeneration,
    /// Final retirement inputs differ from the source and manifest migration validation bound.
    MigrationValidationBindingMismatch,
    /// Destructive legacy-artifact removal lacks an approved release policy.
    ReleasePolicyApprovalRequired,
    /// The declared rollback retention window has not elapsed.
    RollbackWindowActive,
    /// Backup verification is absent before destructive maintenance.
    BackupRequired,
    /// The HNSW artifact and stale vector JSON disposition is not explicit.
    LegacyArtifactPlanIncomplete,
    /// Required authoritative-vector or exact-scan capability would be lost.
    RequiredCapabilityMissing,
    /// A durable generation identity is empty or malformed.
    InvalidIdentity,
    /// A durable manifest is internally inconsistent or unsupported.
    InvalidManifest,
}

impl LegacyRetirementDiagnosticCode {
    /// Returns the stable machine-readable diagnostic string.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DiskAnn3CompileOnly => "diskann3_compile_only",
            Self::MissingVectorSearchConformance => "missing_vector_search_conformance",
            Self::MissingExactIdentityValidation => "missing_exact_identity_validation",
            Self::MissingAuthorizedSubsetRecall => "missing_authorized_subset_recall",
            Self::MissingResourceGate => "missing_resource_gate",
            Self::MissingLatencyConcurrencyGate => "missing_latency_concurrency_gate",
            Self::MissingMutationRecoveryGate => "missing_mutation_recovery_gate",
            Self::MissingDualGenerationShadow => "missing_dual_generation_shadow",
            Self::MissingRollbackDisasterRecovery => "missing_rollback_disaster_recovery",
            Self::MissingOperatorReadiness => "missing_operator_readiness",
            Self::MigrationValidationIncomplete => "migration_validation_incomplete",
            Self::DimensionReductionForbidden => "dimension_reduction_forbidden",
            Self::SilentReembeddingForbidden => "silent_reembedding_forbidden",
            Self::ShadowComparisonIncomplete => "shadow_comparison_incomplete",
            Self::ShadowComparisonFailed => "shadow_comparison_failed",
            Self::PromotionGenerationMismatch => "promotion_generation_mismatch",
            Self::ShadowComparisonSameGeneration => "shadow_comparison_same_generation",
            Self::MigrationValidationBindingMismatch => "migration_validation_binding_mismatch",
            Self::ReleasePolicyApprovalRequired => "release_policy_approval_required",
            Self::RollbackWindowActive => "rollback_window_active",
            Self::BackupRequired => "backup_required",
            Self::LegacyArtifactPlanIncomplete => "legacy_artifact_plan_incomplete",
            Self::RequiredCapabilityMissing => "required_capability_missing",
            Self::InvalidIdentity => "invalid_identity",
            Self::InvalidManifest => "invalid_manifest",
        }
    }
}

/// A diagnostic-only legacy-retirement error.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct LegacyRetirementError {
    code: LegacyRetirementDiagnosticCode,
}

impl LegacyRetirementError {
    /// Builds a code-only contract error.
    pub const fn contract(code: LegacyRetirementDiagnosticCode) -> Self {
        Self { code }
    }

    /// Returns the closed diagnostic code.
    pub const fn diagnostic_code(self) -> LegacyRetirementDiagnosticCode {
        self.code
    }
}

impl fmt::Debug for LegacyRetirementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "LegacyRetirementError({})",
            self.diagnostic_code().as_str()
        )
    }
}

impl fmt::Display for LegacyRetirementError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "legacy-vector-cutover.{}",
            self.diagnostic_code().as_str()
        )
    }
}

impl Error for LegacyRetirementError {}
