//! Fail-closed, diagnostic-only failures for the immutable vector-shard contract.
//!
//! No variant retains a caller-controlled identifier, file name, checksum, tenant,
//! ACL, or source. Public `Debug` and `Display` rendering emit only the closed code,
//! so the failure is safe to surface in operational diagnostics.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Result alias for immutable vector-shard contract operations.
pub type VectorShardResult<T> = Result<T, VectorShardError>;

/// Closed diagnostic taxonomy. No variant retains caller-controlled input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VectorShardDiagnosticCode {
    InvalidShardId,
    InvalidGeneration,
    InvalidFileHash,
    InvalidManifest,
    InvalidFileSet,
    InvalidGrowthBound,
    InvalidRouter,
    InvalidRouterSelection,
    FanOutExceeded,
    DeadlineExceeded,
    InvalidCheckpoint,
    CheckpointNotDurable,
    SerializationFailed,
}

impl VectorShardDiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidShardId => "invalid_shard_id",
            Self::InvalidGeneration => "invalid_generation",
            Self::InvalidFileHash => "invalid_file_hash",
            Self::InvalidManifest => "invalid_manifest",
            Self::InvalidFileSet => "invalid_file_set",
            Self::InvalidGrowthBound => "invalid_growth_bound",
            Self::InvalidRouter => "invalid_router",
            Self::InvalidRouterSelection => "invalid_router_selection",
            Self::FanOutExceeded => "fan_out_exceeded",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::InvalidCheckpoint => "invalid_checkpoint",
            Self::CheckpointNotDurable => "checkpoint_not_durable",
            Self::SerializationFailed => "serialization_failed",
        }
    }
}

/// A vector-shard contract failure containing only a closed diagnostic code.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum VectorShardError {
    Contract { code: VectorShardDiagnosticCode },
}

impl VectorShardError {
    pub const fn contract(code: VectorShardDiagnosticCode) -> Self {
        Self::Contract { code }
    }

    pub const fn diagnostic_code(self) -> VectorShardDiagnosticCode {
        match self {
            Self::Contract { code } => code,
        }
    }
}

impl fmt::Debug for VectorShardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "VectorShardError({})",
            self.diagnostic_code().as_str()
        )
    }
}

impl fmt::Display for VectorShardError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "vector-shards.{}",
            self.diagnostic_code().as_str()
        )
    }
}

impl Error for VectorShardError {}
