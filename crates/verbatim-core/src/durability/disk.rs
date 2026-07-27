//! Disk-reserve and disk-full fail-closed policy.

use serde::{Deserialize, Serialize};

use super::{DurabilityDiagnosticCode, DurabilityError, DurabilityProfile, DurabilityResult};

/// Disk-full handling is deliberately not configurable to a best-effort mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskFullBehavior {
    RejectWritePreserveActiveGeneration,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskFullSignal {
    SqliteFull,
    Enospc,
}

/// Per-profile local-disk reserve. SQLite database, WAL, and rollback files must
/// share one host and local filesystem; a future adapter owns measurement.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiskSpacePolicy {
    pub reserve_bytes: u64,
    pub alert_threshold_bytes: u64,
    pub full_behavior: DiskFullBehavior,
}

impl DiskSpacePolicy {
    pub const fn for_profile(profile: DurabilityProfile) -> Self {
        match profile {
            DurabilityProfile::Durable => Self {
                reserve_bytes: 1_073_741_824,
                alert_threshold_bytes: 2_147_483_648,
                full_behavior: DiskFullBehavior::RejectWritePreserveActiveGeneration,
            },
            DurabilityProfile::Balanced => Self {
                reserve_bytes: 536_870_912,
                alert_threshold_bytes: 1_073_741_824,
                full_behavior: DiskFullBehavior::RejectWritePreserveActiveGeneration,
            },
            DurabilityProfile::Ephemeral => Self {
                reserve_bytes: 67_108_864,
                alert_threshold_bytes: 134_217_728,
                full_behavior: DiskFullBehavior::RejectWritePreserveActiveGeneration,
            },
        }
    }

    pub fn validate(&self) -> DurabilityResult<()> {
        if self.reserve_bytes == 0 {
            return Err(DurabilityError::validation(
                DurabilityDiagnosticCode::DiskReserveMustBePositive,
            ));
        }
        if self.alert_threshold_bytes < self.reserve_bytes {
            return Err(DurabilityError::validation(
                DurabilityDiagnosticCode::DiskAlertThresholdInvalid,
            ));
        }
        if self.full_behavior != DiskFullBehavior::RejectWritePreserveActiveGeneration {
            return Err(DurabilityError::validation(
                DurabilityDiagnosticCode::DiskFullBehaviorMustPreserveActiveGeneration,
            ));
        }
        Ok(())
    }

    /// Reject writes before consuming the generation-preservation reserve.
    pub fn preflight(&self, available_bytes: u64) -> DurabilityResult<()> {
        self.validate()?;
        if available_bytes < self.reserve_bytes {
            return Err(DurabilityError::validation(
                DurabilityDiagnosticCode::DiskReserveNotMet,
            ));
        }
        Ok(())
    }

    /// Report a disk-full signal without offering a corruption-prone fallback.
    pub const fn fail_closed(&self, signal: DiskFullSignal) -> DurabilityResult<()> {
        match signal {
            DiskFullSignal::SqliteFull => Err(DurabilityError::validation(
                DurabilityDiagnosticCode::SqliteFullFailClosed,
            )),
            DiskFullSignal::Enospc => Err(DurabilityError::validation(
                DurabilityDiagnosticCode::EnospcFailClosed,
            )),
        }
    }
}
