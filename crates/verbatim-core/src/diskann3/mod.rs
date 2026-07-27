//! DiskANN3 SSD-native vector retrieval architecture contract (issue #369).
//!
//! This pure walking skeleton defines typed vector-space, shard, resource,
//! filtering, publication, and retrieval-stage boundaries. It deliberately has
//! no DiskANN3, HNSW, Qdrant, LanceDB, SQLite, filesystem, daemon, or provider
//! integration. See `docs/architecture/diskann3-architecture.md`.

mod backend;
mod budget;
mod contract;
mod dimension;
mod error;
mod filter;
mod retrieval;
mod shard;

pub use backend::{BackendRole, BackendSelection, VectorBackend};
pub use budget::{
    BudgetUsage, ConcurrencyBudget, IoBudget, MemoryBudget, RetrievalStageBudget, SearchBudget,
    MAX_PEAK_MEMORY_BYTES, MEBIBYTE,
};
pub use contract::{
    decode_bounded_candidates_json, decode_search_budget_json, encode_bounded_candidates_json,
    encode_search_budget_json, VectorSearchContract, VectorSearchPolicy,
};
pub use dimension::VectorDimension;
pub use error::{VectorSearchDiagnosticCode, VectorSearchError, VectorSearchResult};
pub use filter::{FilterPredicate, LifecycleState, TypedMetadataValue};
pub use retrieval::{
    BoundedCandidates, CandidateGenerationPath, ExactScanThreshold, FilteredCandidates,
    FusedCandidates, GeneratedCandidates, HydratedCandidates, RerankedCandidates,
    RescoredCandidates, RetrievalStage, VectorCandidate,
};
pub use shard::{
    decode_ssd_shard_manifest_json, encode_ssd_shard_manifest_json, PublicationGeneration,
    QuantizerType, ShardChecksum, ShardId, SsdPageLayout, SsdShardManifest, SsdShardManifestFields,
    VectorSpaceId,
};

/// Contract schema version for DiskANN3 architecture documents.
pub const DISKANN3_CONTRACT_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
#[path = "../diskann3_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "../diskann3_stage_contract_tests.rs"]
mod stage_contract_tests;
