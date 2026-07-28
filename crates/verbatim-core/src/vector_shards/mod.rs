//! Immutable SSD-native vector shard contract (Refs #373).
//!
//! This pure walking skeleton defines the physical shard, identifier, file-layout,
//! manifest, routing, and compaction model for DiskANN3 so SSD usage grows
//! linearly with vectors and metadata while online memory and open-file state
//! stay bounded. It deliberately has no live SSD I/O, no DiskANN3 dependency, no
//! upstream binding, no filesystem, daemon, or provider integration. See
//! `docs/architecture/immutable-vector-shards.md`.
//!
//! ## Fail-closed contract surface
//!
//! All validation rejects invalid input. Errors are diagnostic-code-only: no
//! variant retains a caller-controlled identifier, file name, checksum, tenant,
//! ACL, or source. Public `Debug` and `Display` emit only the closed code.
//!
//! ## Complexity bounds
//!
//! Documented and tested upper bounds in terms of N vectors, dimension D, fixed
//! graph degree R, candidate-code bytes Q, and metadata M:
//!
//! ```text
//! original vectors:  O(N * D)
//! graph/pages:       O(N * R)
//! candidate codes:   O(N * Q)
//! metadata/filter:   O(N + M)
//! manifests/maps:    O(N)
//! ```
//!
//! No component may contain all source pairs, tenant pairs, vector pairs, or
//! per-source graph copies.

mod checkpoint;
mod contract;
mod error;
mod identity;
mod manifest;
mod router;

pub use checkpoint::{FsyncAttestation, ShardBuildCheckpoint, ShardBuildStage};
pub use contract::{decode_shard_manifest_json, encode_shard_manifest_json};
pub use error::{VectorShardDiagnosticCode, VectorShardError, VectorShardResult};
pub use identity::{ShardGeneration, ShardId, ShardOrdinal, ShardVectorSpace};
pub use manifest::{
    FileHash, ShardFile, ShardFileName, ShardFileRole, ShardManifest, StorageGrowthBound,
    REQUIRED_ROLES,
};
pub use router::{GenerationDescriptor, ShardRouter, ShardRouterConfig};

/// Contract schema version for immutable vector-shard documents.
pub const IMMUTABLE_VECTOR_SHARD_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
#[path = "../vector_shards_tests.rs"]
mod tests;
