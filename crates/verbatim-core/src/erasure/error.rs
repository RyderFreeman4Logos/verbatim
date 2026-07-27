//! Typed diagnostic-only failures for the erasure contract.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

pub type ErasureResult<T> = Result<T, ErasureError>;

/// Closed diagnostic codes. No error carries source text, identifiers, paths,
/// backend responses, or other caller-controlled strings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ErasureDiagnosticCode {
    ScopeSourceIdsRequired,
    ScopeSourceIdInvalid,
    ScopeSourceIdsDuplicate,
    ScopeTargetSetIncomplete,
    ScopeProductMismatch,
    ScopeInitialStateInvalid,
    PolicyStaleReadFenceRequired,
    PolicyPropagationIncomplete,
    LegalHoldBlocksDeletion,
    MatrixCoverageMissing,
    MatrixClassificationMismatch,
    MatrixOrderingInvalid,
    RetryAttemptsMustBePositive,
    RetryDelayMustBePositive,
    RemoteTargetRequired,
    RemoteFailureDeadLetterRequired,
    RemoteFailureOperatorAlertRequired,
    CryptographicErasureKeyRotationRequired,
    PropagationOrderInvalid,
    PropagationCoverageInvalid,
    ReconciliationMismatch,
    PlanSerializationFailed,
    InvalidPlanJson,
    ProofInvalid,
}

impl ErasureDiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ScopeSourceIdsRequired => "scope_source_ids_required",
            Self::ScopeSourceIdInvalid => "scope_source_id_invalid",
            Self::ScopeSourceIdsDuplicate => "scope_source_ids_duplicate",
            Self::ScopeTargetSetIncomplete => "scope_target_set_incomplete",
            Self::ScopeProductMismatch => "scope_product_mismatch",
            Self::ScopeInitialStateInvalid => "scope_initial_state_invalid",
            Self::PolicyStaleReadFenceRequired => "policy_stale_read_fence_required",
            Self::PolicyPropagationIncomplete => "policy_propagation_incomplete",
            Self::LegalHoldBlocksDeletion => "legal_hold_blocks_deletion",
            Self::MatrixCoverageMissing => "matrix_coverage_missing",
            Self::MatrixClassificationMismatch => "matrix_classification_mismatch",
            Self::MatrixOrderingInvalid => "matrix_ordering_invalid",
            Self::RetryAttemptsMustBePositive => "retry_attempts_must_be_positive",
            Self::RetryDelayMustBePositive => "retry_delay_must_be_positive",
            Self::RemoteTargetRequired => "remote_target_required",
            Self::RemoteFailureDeadLetterRequired => "remote_failure_dead_letter_required",
            Self::RemoteFailureOperatorAlertRequired => "remote_failure_operator_alert_required",
            Self::CryptographicErasureKeyRotationRequired => {
                "cryptographic_erasure_key_rotation_required"
            }
            Self::PropagationOrderInvalid => "propagation_order_invalid",
            Self::PropagationCoverageInvalid => "propagation_coverage_invalid",
            Self::ReconciliationMismatch => "reconciliation_mismatch",
            Self::PlanSerializationFailed => "plan_serialization_failed",
            Self::InvalidPlanJson => "invalid_plan_json",
            Self::ProofInvalid => "proof_invalid",
        }
    }
}

/// An erasure failure consists only of a closed diagnostic code.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum ErasureError {
    Validation { code: ErasureDiagnosticCode },
}

impl ErasureError {
    pub const fn validation(code: ErasureDiagnosticCode) -> Self {
        Self::Validation { code }
    }

    pub const fn diagnostic_code(self) -> ErasureDiagnosticCode {
        match self {
            Self::Validation { code } => code,
        }
    }
}

impl fmt::Debug for ErasureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "ErasureError({})",
            self.diagnostic_code().as_str()
        )
    }
}

impl fmt::Display for ErasureError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "erasure.{}", self.diagnostic_code().as_str())
    }
}

impl Error for ErasureError {}
