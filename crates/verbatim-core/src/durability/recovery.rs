//! Abnormal-shutdown recovery checks.

use serde::{Deserialize, Serialize};

use super::{DurabilityDiagnosticCode, DurabilityError, DurabilityProfile, DurabilityResult};

/// Recovery requirements a future adapter must perform after detecting abnormal
/// shutdown. Normal clean starts remain outside this pure policy contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryPolicy {
    pub run_integrity_check_after_abnormal_shutdown: bool,
    pub run_foreign_key_check_after_abnormal_shutdown: bool,
}

impl RecoveryPolicy {
    pub const fn for_profile(profile: DurabilityProfile) -> Self {
        match profile {
            DurabilityProfile::Durable | DurabilityProfile::Balanced => Self {
                run_integrity_check_after_abnormal_shutdown: true,
                run_foreign_key_check_after_abnormal_shutdown: true,
            },
            DurabilityProfile::Ephemeral => Self {
                run_integrity_check_after_abnormal_shutdown: false,
                run_foreign_key_check_after_abnormal_shutdown: false,
            },
        }
    }

    pub fn validate_for(&self, profile: DurabilityProfile) -> DurabilityResult<()> {
        if matches!(
            profile,
            DurabilityProfile::Durable | DurabilityProfile::Balanced
        ) && !self.run_integrity_check_after_abnormal_shutdown
        {
            return Err(DurabilityError::validation(
                DurabilityDiagnosticCode::RecoveryIntegrityCheckRequired,
            ));
        }
        if matches!(
            profile,
            DurabilityProfile::Durable | DurabilityProfile::Balanced
        ) && !self.run_foreign_key_check_after_abnormal_shutdown
        {
            return Err(DurabilityError::validation(
                DurabilityDiagnosticCode::RecoveryForeignKeyCheckRequired,
            ));
        }
        Ok(())
    }
}
