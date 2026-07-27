//! Named durability profiles and their explicit defaults.

use serde::{Deserialize, Serialize};

use super::{
    CheckpointInterval, CheckpointMode, Dr001BackupRequirement, DurabilityConfig, JournalMode,
    RpoContract, RpoGuarantee, SynchronousMode,
};

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DurabilityProfile {
    Durable,
    #[default]
    Balanced,
    Ephemeral,
}

impl DurabilityProfile {
    pub const ALL: [Self; 3] = [Self::Durable, Self::Balanced, Self::Ephemeral];

    pub const fn default_config(self) -> DurabilityConfig {
        match self {
            Self::Durable => DurabilityConfig {
                profile: Self::Durable,
                journal_mode: JournalMode::Wal,
                synchronous: SynchronousMode::Full,
                wal_autocheckpoint_pages: 1_000,
                busy_timeout_ms: 30_000,
                checkpoint_interval: CheckpointInterval {
                    mode: CheckpointMode::Full,
                    interval_seconds: 30,
                },
            },
            Self::Balanced => DurabilityConfig {
                profile: Self::Balanced,
                journal_mode: JournalMode::Wal,
                synchronous: SynchronousMode::Normal,
                wal_autocheckpoint_pages: 1_000,
                busy_timeout_ms: 10_000,
                checkpoint_interval: CheckpointInterval {
                    mode: CheckpointMode::Passive,
                    interval_seconds: 60,
                },
            },
            Self::Ephemeral => DurabilityConfig {
                profile: Self::Ephemeral,
                journal_mode: JournalMode::Delete,
                synchronous: SynchronousMode::Off,
                wal_autocheckpoint_pages: 100,
                busy_timeout_ms: 1_000,
                checkpoint_interval: CheckpointInterval {
                    mode: CheckpointMode::Truncate,
                    interval_seconds: 300,
                },
            },
        }
    }

    pub const fn rpo_contract(self) -> RpoContract {
        match self {
            Self::Durable => RpoContract {
                profile: Self::Durable,
                rpo: RpoGuarantee::AcknowledgedCommits,
                rto_seconds: 300,
                dr_001_backup: Dr001BackupRequirement::RequiredForHostOrMediaLoss,
            },
            Self::Balanced => RpoContract {
                profile: Self::Balanced,
                rpo: RpoGuarantee::UnboundedPowerLoss,
                rto_seconds: 600,
                dr_001_backup: Dr001BackupRequirement::RequiredForHostOrMediaLoss,
            },
            Self::Ephemeral => RpoContract {
                profile: Self::Ephemeral,
                rpo: RpoGuarantee::NoGuarantee,
                rto_seconds: 900,
                dr_001_backup: Dr001BackupRequirement::RequiredForHostOrMediaLoss,
            },
        }
    }
}
