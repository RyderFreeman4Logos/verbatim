//! Closed, redacted diagnostics for the DiskANN3 retrieval service contract.

use std::error::Error;
use std::fmt;

/// Result alias for DiskANN3 retrieval-service contract operations.
pub type DiskAnn3ServiceResult<T> = Result<T, DiskAnn3ServiceError>;

/// Closed diagnostic taxonomy. No variant retains request, tenant, shard, or endpoint data.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DiskAnn3ServiceDiagnosticCode {
    InvalidIdentity,
    InvalidRequest,
    InvalidPredicate,
    InvalidAuthorization,
    AuthorizationUncertain,
    InvalidShardMetadata,
    StaleGeneration,
    GenerationMismatch,
    FanOutExceeded,
    PartialShardUnavailable,
    IncompatibleActiveGeneration,
    InvalidReplicaSet,
    InvalidBackpressureConfig,
    ActiveQueryExceeded,
    QueueExceeded,
    CircuitOpen,
    DeadlineExceeded,
    BudgetExceeded,
    RetryBudgetReset,
    UnsupportedCapability,
    InvalidProtocol,
    InvalidResponse,
    DurabilityContractRequired,
    InvalidDeploymentStorage,
}

impl DiskAnn3ServiceDiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidIdentity => "invalid_identity",
            Self::InvalidRequest => "invalid_request",
            Self::InvalidPredicate => "invalid_predicate",
            Self::InvalidAuthorization => "invalid_authorization",
            Self::AuthorizationUncertain => "authorization_uncertain",
            Self::InvalidShardMetadata => "invalid_shard_metadata",
            Self::StaleGeneration => "stale_generation",
            Self::GenerationMismatch => "generation_mismatch",
            Self::FanOutExceeded => "fan_out_exceeded",
            Self::PartialShardUnavailable => "partial_shard_unavailable",
            Self::IncompatibleActiveGeneration => "incompatible_active_generation",
            Self::InvalidReplicaSet => "invalid_replica_set",
            Self::InvalidBackpressureConfig => "invalid_backpressure_config",
            Self::ActiveQueryExceeded => "active_query_exceeded",
            Self::QueueExceeded => "queue_exceeded",
            Self::CircuitOpen => "circuit_open",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::BudgetExceeded => "budget_exceeded",
            Self::RetryBudgetReset => "retry_budget_reset",
            Self::UnsupportedCapability => "unsupported_capability",
            Self::InvalidProtocol => "invalid_protocol",
            Self::InvalidResponse => "invalid_response",
            Self::DurabilityContractRequired => "durability_contract_required",
            Self::InvalidDeploymentStorage => "invalid_deployment_storage",
        }
    }
}

/// A fail-closed service-contract error that carries only its stable code.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum DiskAnn3ServiceError {
    Contract { code: DiskAnn3ServiceDiagnosticCode },
}

impl DiskAnn3ServiceError {
    pub const fn contract(code: DiskAnn3ServiceDiagnosticCode) -> Self {
        Self::Contract { code }
    }

    pub const fn diagnostic_code(self) -> DiskAnn3ServiceDiagnosticCode {
        match self {
            Self::Contract { code } => code,
        }
    }
}

impl fmt::Debug for DiskAnn3ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "DiskAnn3ServiceError({})",
            self.diagnostic_code().as_str()
        )
    }
}

impl fmt::Display for DiskAnn3ServiceError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "diskann3-service.{}",
            self.diagnostic_code().as_str()
        )
    }
}

impl Error for DiskAnn3ServiceError {}
