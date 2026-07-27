//! Deletion state-machine vocabulary.

use serde::{Deserialize, Serialize};

/// The requested state for a product during an erasure lifecycle. Legal hold is
/// a policy gate (`DeletionPolicy::legal_hold`), never a lifecycle state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionState {
    LogicalDelete,
    Quarantine,
    Tombstone,
    ImmediatePhysicalErase,
    DelayedBackupExpiry,
}

impl DeletionState {
    pub const ALL: [Self; 5] = [
        Self::LogicalDelete,
        Self::Quarantine,
        Self::Tombstone,
        Self::ImmediatePhysicalErase,
        Self::DelayedBackupExpiry,
    ];

    /// Valid lifecycle transitions are forward-only. Terminal cleanup states
    /// cannot advance further.
    pub const fn can_transition_to(self, next: Self) -> bool {
        match self {
            Self::LogicalDelete => matches!(next, Self::Quarantine | Self::Tombstone),
            Self::Quarantine => matches!(next, Self::Tombstone | Self::ImmediatePhysicalErase),
            Self::Tombstone => matches!(
                next,
                Self::ImmediatePhysicalErase | Self::DelayedBackupExpiry
            ),
            Self::ImmediatePhysicalErase | Self::DelayedBackupExpiry => false,
        }
    }
}
