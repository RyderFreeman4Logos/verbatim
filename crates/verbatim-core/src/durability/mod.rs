//! SQLite durability-profile contract (issue #362).
//!
//! This pure walking skeleton defines serializable policy, validation, and an
//! adapter boundary. It deliberately contains no SQLite, Store, filesystem,
//! daemon, or CLI implementation. See `docs/architecture/durability-profiles.md`.

mod config;
mod disk;
mod error;
mod profile;
mod publication;
mod recovery;
mod rpo;
mod workflow;

pub use config::{
    decode_durability_config_json, encode_durability_config_json, CheckpointInterval,
    CheckpointMode, DurabilityConfig, JournalMode, SynchronousMode,
};
pub use disk::{DiskFullBehavior, DiskFullSignal, DiskSpacePolicy};
pub use error::{DurabilityDiagnosticCode, DurabilityError, DurabilityResult};
pub use profile::DurabilityProfile;
pub use publication::{PublicationOrder, PublicationStep};
pub use recovery::RecoveryPolicy;
pub use rpo::{Dr001BackupRequirement, RpoContract, RpoGuarantee};
pub use workflow::DurabilityProfileWorkflow;

/// Contract schema version for durability profile documents.
pub const DURABILITY_CONTRACT_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
#[path = "../durability_tests.rs"]
mod tests;
