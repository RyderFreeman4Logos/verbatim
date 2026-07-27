//! Profile-specific recovery-point and recovery-time contract.

use serde::{Deserialize, Serialize};

use super::{DurabilityDiagnosticCode, DurabilityError, DurabilityProfile, DurabilityResult};

/// Recovery point applies to a power loss on the local host. It does not replace
/// off-host protection for host, media, site, or operator-loss incidents.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RpoGuarantee {
    AcknowledgedCommits,
    UnboundedPowerLoss,
    NoGuarantee,
}

/// DR-001 remains required even for a locally durable SQLite configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Dr001BackupRequirement {
    RequiredForHostOrMediaLoss,
    NotRequired,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RpoContract {
    pub profile: DurabilityProfile,
    pub rpo: RpoGuarantee,
    pub rto_seconds: u64,
    pub dr_001_backup: Dr001BackupRequirement,
}

impl RpoContract {
    pub fn validate(&self) -> DurabilityResult<()> {
        if self.rto_seconds == 0 {
            return Err(DurabilityError::validation(
                DurabilityDiagnosticCode::RtoMustBePositive,
            ));
        }
        if self.dr_001_backup != Dr001BackupRequirement::RequiredForHostOrMediaLoss {
            return Err(DurabilityError::validation(
                DurabilityDiagnosticCode::Dr001BackupRequired,
            ));
        }
        if self != &self.profile.rpo_contract() {
            return Err(DurabilityError::validation(
                DurabilityDiagnosticCode::RpoProfileMismatch,
            ));
        }
        Ok(())
    }
}
