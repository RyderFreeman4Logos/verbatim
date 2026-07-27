//! SQLite pragma and checkpoint scheduling contract.

use serde::{Deserialize, Serialize};

use super::{DurabilityDiagnosticCode, DurabilityError, DurabilityProfile, DurabilityResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum JournalMode {
    Wal,
    Delete,
    Truncate,
}

impl JournalMode {
    pub const ALL: [Self; 3] = [Self::Wal, Self::Delete, Self::Truncate];
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum SynchronousMode {
    Full,
    Normal,
    Off,
}

impl SynchronousMode {
    pub const ALL: [Self; 3] = [Self::Full, Self::Normal, Self::Off];
}

/// The SQLite checkpoint mode scheduled by a future adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "UPPERCASE")]
pub enum CheckpointMode {
    Passive,
    Full,
    Restart,
    Truncate,
}

/// A bounded cadence and mode for checkpoint scheduling.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckpointInterval {
    pub mode: CheckpointMode,
    pub interval_seconds: u64,
}

/// Per-profile SQLite pragma settings. This contract records desired settings;
/// a future SQLite adapter must apply and verify them before accepting writes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DurabilityConfig {
    pub profile: DurabilityProfile,
    pub journal_mode: JournalMode,
    pub synchronous: SynchronousMode,
    pub wal_autocheckpoint_pages: u32,
    pub busy_timeout_ms: u64,
    pub checkpoint_interval: CheckpointInterval,
}

impl DurabilityConfig {
    pub fn validate_for(&self, expected_profile: DurabilityProfile) -> DurabilityResult<()> {
        if self.wal_autocheckpoint_pages == 0 {
            return Err(DurabilityError::validation(
                DurabilityDiagnosticCode::WalAutocheckpointMustBePositive,
            ));
        }
        if self.busy_timeout_ms == 0 {
            return Err(DurabilityError::validation(
                DurabilityDiagnosticCode::BusyTimeoutMustBePositive,
            ));
        }
        if self.checkpoint_interval.interval_seconds == 0 {
            return Err(DurabilityError::validation(
                DurabilityDiagnosticCode::CheckpointIntervalMustBePositive,
            ));
        }
        if self.profile != expected_profile {
            return Err(DurabilityError::validation(
                DurabilityDiagnosticCode::ProfileConfigMismatch,
            ));
        }
        if expected_profile == DurabilityProfile::Durable {
            if self.journal_mode != JournalMode::Wal {
                return Err(DurabilityError::validation(
                    DurabilityDiagnosticCode::DurableRequiresWal,
                ));
            }
            if self.synchronous != SynchronousMode::Full {
                return Err(DurabilityError::validation(
                    DurabilityDiagnosticCode::DurableRequiresFullSynchronous,
                ));
            }
        }

        let expected = expected_profile.default_config();
        if self != &expected {
            return Err(DurabilityError::validation(
                DurabilityDiagnosticCode::ProfileConfigMismatch,
            ));
        }
        Ok(())
    }
}

pub fn encode_durability_config_json(config: &DurabilityConfig) -> DurabilityResult<Vec<u8>> {
    serde_json::to_vec(config).map_err(|_| {
        DurabilityError::validation(DurabilityDiagnosticCode::ConfigSerializationFailed)
    })
}

/// Decode untrusted JSON and validate both the selected profile and all settings
/// before returning a configuration.
pub fn decode_durability_config_json(
    bytes: &[u8],
    expected_profile: DurabilityProfile,
) -> DurabilityResult<DurabilityConfig> {
    let config: DurabilityConfig = serde_json::from_slice(bytes)
        .map_err(|_| DurabilityError::validation(DurabilityDiagnosticCode::InvalidConfigJson))?;
    config.validate_for(expected_profile)?;
    Ok(config)
}
