//! Deletion state-machine vocabulary.

use serde::{Deserialize, Serialize};

/// The requested state for a product during an erasure lifecycle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionState {
    LogicalDelete,
    Quarantine,
    Tombstone,
    ImmediatePhysicalErase,
    DelayedBackupExpiry,
    LegalHold,
}

impl DeletionState {
    pub const ALL: [Self; 6] = [
        Self::LogicalDelete,
        Self::Quarantine,
        Self::Tombstone,
        Self::ImmediatePhysicalErase,
        Self::DelayedBackupExpiry,
        Self::LegalHold,
    ];

    /// Legal hold is terminal for deletion work. It may be entered from an
    /// active lifecycle, but no deletion transition may leave it.
    pub const fn can_transition_to(self, next: Self) -> bool {
        if matches!(self, Self::LegalHold) {
            return false;
        }
        if matches!(next, Self::LegalHold) {
            return true;
        }
        match self {
            Self::LogicalDelete => matches!(next, Self::Quarantine | Self::Tombstone),
            Self::Quarantine => matches!(next, Self::Tombstone | Self::ImmediatePhysicalErase),
            Self::Tombstone => matches!(
                next,
                Self::ImmediatePhysicalErase | Self::DelayedBackupExpiry
            ),
            Self::ImmediatePhysicalErase | Self::DelayedBackupExpiry | Self::LegalHold => false,
        }
    }
}
