//! Adapter boundary for profile resolution, validation, checkpoints, and recovery.

use async_trait::async_trait;

use super::{
    CheckpointInterval, DurabilityConfig, DurabilityProfile, DurabilityResult, RecoveryPolicy,
};

/// Contract-only workflow ordering: resolve a named profile, validate effective
/// settings, checkpoint by the selected schedule, and apply recovery policy.
/// Implementations own SQLite calls and must not continue after any failure.
#[async_trait]
pub trait DurabilityProfileWorkflow: Send + Sync {
    async fn resolve_profile(
        &self,
        profile: DurabilityProfile,
    ) -> DurabilityResult<DurabilityConfig>;

    async fn validate_config(&self, config: &DurabilityConfig) -> DurabilityResult<()>;

    async fn checkpoint(
        &self,
        config: &DurabilityConfig,
        schedule: CheckpointInterval,
    ) -> DurabilityResult<()>;

    async fn recover(
        &self,
        profile: DurabilityProfile,
        recovery_policy: RecoveryPolicy,
        abnormal_shutdown: bool,
    ) -> DurabilityResult<()>;
}
