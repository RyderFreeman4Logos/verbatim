//! Fail-closed, diagnostic-only failures for the durable update lifecycle contract.
//!
//! No variant retains a caller-controlled identifier, vector, source, tenant, ACL,
//! content hash, or checkpoint path. Public `Debug` and `Display` rendering emit
//! only the closed diagnostic code, so the failure is safe to surface in
//! operational diagnostics.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Result alias for durable update contract operations.
pub type DurableUpdateResult<T> = Result<T, DurableUpdateError>;

/// Closed diagnostic taxonomy. No variant retains caller-controlled input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurableUpdateDiagnosticCode {
    /// A mutation batch was empty or exceeded its documented bound.
    InvalidMutationBatch,
    /// A batch contained two operations targeting the same vector identity.
    DuplicateMutationVectorId,
    /// A vector id, generation, or version was zero or otherwise malformed.
    InvalidIdentity,
    /// The supplied operation was not the next in the monotonic version order.
    VersionOutOfOrder,
    /// A replayed idempotency key conflicted with a prior committed effect.
    IdempotencyConflict,
    /// The operation targeted a generation that is no longer the live generation.
    StaleGeneration,
    /// A tombstone batch exceeded the capped tombstone/delta memory bound.
    TombstoneCapExceeded,
    /// A compaction trigger or plan was malformed or internally inconsistent.
    InvalidCompactionPlan,
    /// Recovery inspected an inconsistent manifest that may not be published.
    InconsistentRecovery,
    /// Old-page reclamation was attempted while a generation/query lease was live.
    LeaseActive,
    /// A checkpoint lacked the fsync attestation its stage requires.
    CheckpointNotDurable,
    /// Source replacement exposed old and new chunks together under one generation.
    SourceReplaceVisibilityViolation,
    /// Referential validation against authoritative catalog/evidence state failed.
    ReferentialValidationFailed,
    /// JSON encoding or decoding of a contract document failed.
    SerializationFailed,
}

impl DurableUpdateDiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidMutationBatch => "invalid_mutation_batch",
            Self::DuplicateMutationVectorId => "duplicate_mutation_vector_id",
            Self::InvalidIdentity => "invalid_identity",
            Self::VersionOutOfOrder => "version_out_of_order",
            Self::IdempotencyConflict => "idempotency_conflict",
            Self::StaleGeneration => "stale_generation",
            Self::TombstoneCapExceeded => "tombstone_cap_exceeded",
            Self::InvalidCompactionPlan => "invalid_compaction_plan",
            Self::InconsistentRecovery => "inconsistent_recovery",
            Self::LeaseActive => "lease_active",
            Self::CheckpointNotDurable => "checkpoint_not_durable",
            Self::SourceReplaceVisibilityViolation => "source_replace_visibility_violation",
            Self::ReferentialValidationFailed => "referential_validation_failed",
            Self::SerializationFailed => "serialization_failed",
        }
    }
}

/// A durable-update contract failure containing only a closed diagnostic code.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum DurableUpdateError {
    Contract { code: DurableUpdateDiagnosticCode },
}

impl DurableUpdateError {
    pub const fn contract(code: DurableUpdateDiagnosticCode) -> Self {
        Self::Contract { code }
    }

    pub const fn diagnostic_code(self) -> DurableUpdateDiagnosticCode {
        match self {
            Self::Contract { code } => code,
        }
    }
}

impl fmt::Debug for DurableUpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "DurableUpdateError({})",
            self.diagnostic_code().as_str()
        )
    }
}

impl fmt::Display for DurableUpdateError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "durable-updates.{}",
            self.diagnostic_code().as_str()
        )
    }
}

impl Error for DurableUpdateError {}
