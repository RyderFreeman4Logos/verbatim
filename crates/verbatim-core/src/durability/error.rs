//! Typed, diagnostic-only errors for durability-profile contracts.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

pub type DurabilityResult<T> = Result<T, DurabilityError>;

/// Closed diagnostic codes; no error retains untrusted input or operational
/// paths, so diagnostics are safe to serialize and render.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurabilityDiagnosticCode {
    ConfigSerializationFailed,
    InvalidConfigJson,
    WalAutocheckpointMustBePositive,
    BusyTimeoutMustBePositive,
    CheckpointIntervalMustBePositive,
    DurableRequiresWal,
    DurableRequiresFullSynchronous,
    ProfileConfigMismatch,
    DiskReserveMustBePositive,
    DiskAlertThresholdInvalid,
    DiskFullBehaviorMustPreserveActiveGeneration,
    DiskReserveNotMet,
    SqliteFullFailClosed,
    EnospcFailClosed,
    PublicationOrderInvalid,
    RecoveryIntegrityCheckRequired,
    RecoveryForeignKeyCheckRequired,
    RtoMustBePositive,
    Dr001BackupRequired,
    RpoProfileMismatch,
}

impl DurabilityDiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ConfigSerializationFailed => "config_serialization_failed",
            Self::InvalidConfigJson => "invalid_config_json",
            Self::WalAutocheckpointMustBePositive => "wal_autocheckpoint_must_be_positive",
            Self::BusyTimeoutMustBePositive => "busy_timeout_must_be_positive",
            Self::CheckpointIntervalMustBePositive => "checkpoint_interval_must_be_positive",
            Self::DurableRequiresWal => "durable_requires_wal",
            Self::DurableRequiresFullSynchronous => "durable_requires_full_synchronous",
            Self::ProfileConfigMismatch => "profile_config_mismatch",
            Self::DiskReserveMustBePositive => "disk_reserve_must_be_positive",
            Self::DiskAlertThresholdInvalid => "disk_alert_threshold_invalid",
            Self::DiskFullBehaviorMustPreserveActiveGeneration => {
                "disk_full_behavior_must_preserve_active_generation"
            }
            Self::DiskReserveNotMet => "disk_reserve_not_met",
            Self::SqliteFullFailClosed => "sqlite_full_fail_closed",
            Self::EnospcFailClosed => "enospc_fail_closed",
            Self::PublicationOrderInvalid => "publication_order_invalid",
            Self::RecoveryIntegrityCheckRequired => "recovery_integrity_check_required",
            Self::RecoveryForeignKeyCheckRequired => "recovery_foreign_key_check_required",
            Self::RtoMustBePositive => "rto_must_be_positive",
            Self::Dr001BackupRequired => "dr_001_backup_required",
            Self::RpoProfileMismatch => "rpo_profile_mismatch",
        }
    }
}

/// Durable errors contain only a closed diagnostic code; never arbitrary
/// strings, paths, SQL text, filesystem details, or raw operating-system input.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum DurabilityError {
    Validation { code: DurabilityDiagnosticCode },
}

impl DurabilityError {
    pub const fn validation(code: DurabilityDiagnosticCode) -> Self {
        Self::Validation { code }
    }

    const fn diagnostic_code(self) -> DurabilityDiagnosticCode {
        match self {
            Self::Validation { code } => code,
        }
    }
}

impl fmt::Debug for DurabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DurabilityError({})", self.diagnostic_code().as_str())
    }
}

impl fmt::Display for DurabilityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "durability.{}", self.diagnostic_code().as_str())
    }
}

impl Error for DurabilityError {}
