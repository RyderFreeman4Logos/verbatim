//! Retention, stale-read, and ACL propagation policy.

use serde::{Deserialize, Serialize};

use super::{ErasureDiagnosticCode, ErasureError, ErasureResult};

/// Required propagation of revocation/retention policy beyond stored content.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyPropagation {
    pub cache_keys: bool,
    pub active_cursors: bool,
    pub derived_artifacts: bool,
    pub model_eligibility: bool,
}

impl PolicyPropagation {
    pub const fn required() -> Self {
        Self {
            cache_keys: true,
            active_cursors: true,
            derived_artifacts: true,
            model_eligibility: true,
        }
    }

    pub const fn is_complete(self) -> bool {
        self.cache_keys && self.active_cursors && self.derived_artifacts && self.model_eligibility
    }
}

/// Key-management requirement used when backups cannot be physically rewritten
/// within the required erasure window.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyRotationRequirement {
    Required,
    NotApplicable,
}

/// Documented cryptographic-erasure fallback for immutable or impractical
/// backup rewrite media. A future adapter must rotate and revoke the data key.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CryptographicErasure {
    pub backup_rewrite_impractical: bool,
    pub key_rotation: KeyRotationRequirement,
}

impl Default for CryptographicErasure {
    fn default() -> Self {
        Self {
            backup_rewrite_impractical: true,
            key_rotation: KeyRotationRequirement::Required,
        }
    }
}

impl CryptographicErasure {
    pub fn validate(self) -> ErasureResult<()> {
        if self.backup_rewrite_impractical && self.key_rotation != KeyRotationRequirement::Required
        {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::CryptographicErasureKeyRotationRequired,
            ));
        }
        Ok(())
    }
}

/// Retention rules and mandatory revocation propagation for one plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionPolicy {
    /// Denies all reads for the requested scope until reconciliation completes.
    pub stale_read_fence: bool,
    /// Required ACL/retention propagation surfaces.
    pub propagation: PolicyPropagation,
    /// How long a retained audit record may remain in backup media.
    pub backup_retention_seconds: u64,
    /// Legal hold takes precedence over every deletion state and backend.
    pub legal_hold: bool,
    pub cryptographic_erasure: CryptographicErasure,
}

impl Default for DeletionPolicy {
    fn default() -> Self {
        Self {
            stale_read_fence: true,
            propagation: PolicyPropagation::required(),
            backup_retention_seconds: 86_400,
            legal_hold: false,
            cryptographic_erasure: CryptographicErasure::default(),
        }
    }
}

impl DeletionPolicy {
    pub fn validate(self) -> ErasureResult<()> {
        if self.legal_hold {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::LegalHoldBlocksDeletion,
            ));
        }
        if !self.stale_read_fence {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::PolicyStaleReadFenceRequired,
            ));
        }
        if !self.propagation.is_complete() {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::PolicyPropagationIncomplete,
            ));
        }
        self.cryptographic_erasure.validate()
    }
}
