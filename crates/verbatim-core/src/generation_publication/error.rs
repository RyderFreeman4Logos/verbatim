//! Fail-closed, diagnostic-only failures for the generation publication and
//! migration contract.
//!
//! No variant retains a caller-controlled identifier, vector, source, tenant,
//! ACL, content hash, shard id, embedding profile, or manifest path. Public
//! `Debug` and `Display` rendering emit only the closed diagnostic code, so the
//! failure is safe to surface in operational diagnostics.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Result alias for generation publication contract operations.
pub type GenerationPublicationResult<T> = Result<T, GenerationPublicationError>;

/// Closed diagnostic taxonomy. No variant retains caller-controlled input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GenerationPublicationDiagnosticCode {
    /// A manifest, pointer, lease, or receipt was malformed or internally
    /// inconsistent.
    InvalidContract,
    /// A generation, version, or sequence number was zero or otherwise invalid.
    InvalidIdentity,
    /// A content or file hash failed `sha256:` revalidation.
    InvalidHash,
    /// A bounded range, shard list, or count was empty or exceeded its bound.
    InvalidBounds,
    /// A declared capability lacked its required component digest.
    MissingComponent,
    /// Two staged generations referenced the same shard ordinal or identity.
    DuplicateShard,
    /// Promotion was attempted on a generation that is not `Ready`.
    NotPromotable,
    /// The observed stage does not permit the requested transition.
    InvalidStageTransition,
    /// The active pointer CAS did not match (concurrent promotion/rollback).
    PointerConflict,
    /// A coordinator lock is held by a different promotion epoch.
    CoordinatorLocked,
    /// A generation was referenced that is no longer the active generation.
    StaleGeneration,
    /// Old-generation reclamation was attempted while a lease was still live.
    LeaseActive,
    /// A staged generation claimed fsync durability without an attestation.
    StagingNotDurable,
    /// A quarantine record conflicts with a later promotion/ACL generation.
    QuarantineConflict,
    /// A rollback receipt was not durable across the observed restart.
    RollbackNotDurable,
    /// A migration profile mixed candidate backends without explicit fusion.
    MixedGenerationRead,
    /// Sampled recall or resource validation failed before promotion.
    QualityGateFailed,
    /// A backends' compatibility or minimum reader version is insufficient.
    IncompatibleBackend,
    /// JSON encoding or decoding of a contract document failed.
    SerializationFailed,
}

impl GenerationPublicationDiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidContract => "invalid_contract",
            Self::InvalidIdentity => "invalid_identity",
            Self::InvalidHash => "invalid_hash",
            Self::InvalidBounds => "invalid_bounds",
            Self::MissingComponent => "missing_component",
            Self::DuplicateShard => "duplicate_shard",
            Self::NotPromotable => "not_promotable",
            Self::InvalidStageTransition => "invalid_stage_transition",
            Self::PointerConflict => "pointer_conflict",
            Self::CoordinatorLocked => "coordinator_locked",
            Self::StaleGeneration => "stale_generation",
            Self::LeaseActive => "lease_active",
            Self::StagingNotDurable => "staging_not_durable",
            Self::QuarantineConflict => "quarantine_conflict",
            Self::RollbackNotDurable => "rollback_not_durable",
            Self::MixedGenerationRead => "mixed_generation_read",
            Self::QualityGateFailed => "quality_gate_failed",
            Self::IncompatibleBackend => "incompatible_backend",
            Self::SerializationFailed => "serialization_failed",
        }
    }
}

/// A generation-publication contract failure containing only a closed diagnostic
/// code.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum GenerationPublicationError {
    Contract {
        code: GenerationPublicationDiagnosticCode,
    },
}

impl GenerationPublicationError {
    pub const fn contract(code: GenerationPublicationDiagnosticCode) -> Self {
        Self::Contract { code }
    }

    pub const fn diagnostic_code(self) -> GenerationPublicationDiagnosticCode {
        match self {
            Self::Contract { code } => code,
        }
    }
}

impl fmt::Debug for GenerationPublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "GenerationPublicationError({})",
            self.diagnostic_code().as_str()
        )
    }
}

impl fmt::Display for GenerationPublicationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "generation-publication.{}",
            self.diagnostic_code().as_str()
        )
    }
}

impl Error for GenerationPublicationError {}
