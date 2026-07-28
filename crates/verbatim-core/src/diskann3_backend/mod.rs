//! Thin DiskANN3 adapter contract, separate from the architecture contract.
//!
//! This module defines validation and operation boundaries only. It intentionally
//! contains no upstream DiskANN3 dependency, SSD I/O, daemon wiring, or ANN core.

mod capability;
mod context;
mod contract;
mod error;
mod identity;
mod input;
mod lifecycle;
mod mutation;
mod quality;
mod search;
mod space;

pub use crate::search_planner::{SearchBudget, SearchBudgetFields};

pub use capability::{
    DiskAnnCapabilities, DiskAnnCapabilityFields, PageCacheDiagnosticFields, PageCacheDiagnostics,
};

pub use context::{GenerationContext, PredicatePlan, SearchBudgetBinding, SearchContext};
pub use contract::DiskAnnVectorSearch;
pub use error::{DiskAnnBackendDiagnosticCode, DiskAnnBackendError, DiskAnnBackendResult};
pub use identity::{ChunkIdMapping, ChunkIdMappingEntry, MappingVersion, StableVectorId};
pub use input::VectorInput;
pub use lifecycle::{
    GenerationLifecycleState, GenerationReceipt, GenerationStatus, RecoverySource,
    RestoreOrRebuildRequest, ShardGenerationRequest, ShutdownReceipt, SnapshotId, SnapshotReceipt,
};
pub use mutation::{
    BatchUpsertRequest, IdempotencyKey, MutationReceipt, TombstoneBatchRequest, VectorUpsert,
};
pub use quality::{
    CandidateRepresentation, CandidateScore, ExactRescoreCandidate, ExactVector,
    ExactVectorFetchRequest, ExactVectorFetchResponse, FullQualityGuarantee,
};
pub use search::{
    RangeSearchRequest, RawDistanceRange, SearchCandidate, SearchPage, TopKSearchRequest,
};
pub use space::{VectorMetric, VectorNormalization, VectorSpaceSpec};

/// Contract schema version for the DiskANN3 backend-adapter boundary.
pub const DISKANN3_BACKEND_ADAPTER_SCHEMA_VERSION: u32 = 1;
