//! Atomic DiskANN3 generation publication and migration contract (Refs #379).
//!
//! This pure contract module defines the publication manifest, lifecycle,
//! pointer, lease, coordinator-lock, quarantine, rollback-durability, and
//! dual-generation migration types required for atomic DiskANN3 generation
//! publication without mixed-index reads. It deliberately contains no live
//! SSD I/O, no DiskANN3 binding, no migration daemon, and no provider
//! integration. See `docs/architecture/generation-publication.md`.
//!
//! ## Fail-closed contract surface
//!
//! All validation rejects invalid input. Errors are diagnostic-code-only: no
//! variant retains a caller-controlled identifier, vector, content hash,
//! tenant, ACL, shard id, embedding profile, or manifest path. Public `Debug`
//! and `Display` emit only the closed code.
//!
//! ## Lifecycle
//!
//! ```text
//! authoritative snapshot fixed
//!   → stage lexical/vector/filter/graph artifacts
//!   → fsync / checksum / validate
//!   → run conformance and sampled quality gates
//!   → create publication manifest
//!   → atomically promote active pointer
//!   → serve queries bound to that generation
//!   → retain old generation for leases / cursors / rollback
//!   → bounded garbage collection
//! ```
//!
//! ## Migration
//!
//! Dual-generation evaluation shadows the incumbent and candidate under
//! mirrored sampled queries with independent metrics. Fusion of old/new backend
//! results is never the default; it is an explicit experiment opt-in.
//!
//! ## Failure handling
//!
//! Startup reconciliation checks every manifest and backend generation;
//! incomplete/corrupt generations are quarantined. Two coordinators cannot
//! promote different generations concurrently. Rollback is durable across
//! restart.

mod error;
mod identity;
mod lifecycle;
mod manifest;
mod migration;

pub use error::{
    GenerationPublicationDiagnosticCode, GenerationPublicationError, GenerationPublicationResult,
};
pub use identity::{ContentHash, CoordinatorEpoch, PublicationGenerationId, ShardOrdinal};
pub use lifecycle::{
    can_promote, validate_stage_transition, GenerationLease, LeaseRegistry, PublicationPointer,
};
pub use manifest::{
    decode_publication_manifest_json, encode_publication_manifest_json, BuildResourceReport,
    CandidateQuantizer, CompatibilityContract, FilterAclBinding, OriginalVectorEncoding,
    PublicationManifest, PublicationStage, SampledRecallReport, ShardDescriptor, UpdateCheckpoint,
    VectorBackendProvider, VectorMetric, VectorNormalization,
    GENERATION_PUBLICATION_SCHEMA_VERSION,
};
pub use migration::{
    reject_mixed_generation_read, CoordinatorLock, CoordinatorLockRegistry, FusionPolicy,
    MigrationCandidateMetrics, MigrationProfile, QuarantineRecord, QuarantineRegistry,
    RollbackFsyncAttestation, RollbackReceipt,
};

/// Contract schema version for generation-publication documents.
pub const GENERATION_PUBLICATION_CONTRACT_SCHEMA_VERSION: u32 =
    GENERATION_PUBLICATION_SCHEMA_VERSION;

#[cfg(test)]
#[path = "../generation_publication_tests.rs"]
mod tests;
