//! Durable DiskANN3 update, delete, tombstone, compaction, and crash-recovery
//! contract (Refs #378).
//!
//! This pure walking skeleton defines the mutation lifecycle, tombstone,
//! compaction, lease, and crash-recovery boundaries for durable DiskANN3
//! updates. It deliberately contains no live SSD I/O, no DiskANN3 binding, no
//! compaction daemon, no filesystem, and no provider integration. See
//! `docs/architecture/durable-updates.md`.
//!
//! ## Fail-closed contract surface
//!
//! All validation rejects invalid input. Errors are diagnostic-code-only: no
//! variant retains a caller-controlled identifier, vector, content hash, tenant,
//! ACL, or source. Public `Debug` and `Display` emit only the closed code.
//!
//! ## Mutation lifecycle
//!
//! ```text
//! MutationOperation (Upsert | Delete | Tombstone | SourceReplace)
//!   → MutationBatch (bounded, idempotent, version-ordered)
//!   → stages: OperationLogged → VectorUpserted → Tombstoned
//!       → GraphEdgeUpdated → FilterIndexUpdated → Checkpointed
//!       → Compacted → Validated → Published
//! ```
//!
//! ## Crash model
//!
//! Recovery yields [`CrashRecoveryResult::PreviousCommitted`] or
//! [`CrashRecoveryResult::NewCommitted`] — never a search-visible mixture whose
//! manifest claims success. A state claiming `Published` without full fsync is
//! [`CrashRecoveryResult::InconsistentRejected`] and quarantined.

mod compaction;
mod error;
mod identity;
mod mutation;
mod recovery;
mod tombstone;

pub use compaction::{
    can_reclaim_generation, CompactionPlan, CompactionStage, CompactionThresholds,
    CompactionTrigger, MutationLease,
};
pub use error::{DurableUpdateDiagnosticCode, DurableUpdateError, DurableUpdateResult};
pub use identity::{ContentHash, DurableGeneration, DurableVectorId, MutationVersion};
pub use mutation::{
    MutationBatch, MutationIdempotencyKey, MutationKind, MutationOperation, MutationStage,
};
pub use recovery::{
    validate_source_replace_atomicity, CrashRecoveryResult, RecoveryFsyncAttestation,
};
pub use tombstone::{Tombstone, TombstoneSet};

/// Contract schema version for durable update documents.
pub const DURABLE_UPDATES_CONTRACT_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
#[path = "../durable_updates_tests.rs"]
mod tests;
