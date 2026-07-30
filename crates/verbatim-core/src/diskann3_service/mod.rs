//! DiskANN3 retrieval-service walking-skeleton contract (DIST-SSD-001).
//!
//! This types-only module declares a shared semantic `VectorSearch` boundary for
//! in-process and remote service paths. It owns derived immutable vector shards
//! and routing/filter summaries only: the catalog, evidence database, originals,
//! and hydration remain authoritative services. No live gRPC server, network I/O,
//! DiskANN binding, cgroup enforcement, mutable NFS/SMB index, or daemon exists here.

mod adapter;
mod backpressure;
mod capability;
mod error;
mod identity;
mod protocol;
mod replica;
mod request;
mod response;
mod router;

pub use adapter::{AdapterKind, InProcessAdapter, RemoteAdapter, VectorSearchAdapter};
pub use backpressure::{BackpressureConfig, BackpressureGate, CircuitState, WorkerPool};
pub use capability::ServiceCapabilities;
pub use error::{DiskAnn3ServiceDiagnosticCode, DiskAnn3ServiceError, DiskAnn3ServiceResult};
pub use identity::{
    Generation, IdempotencyKey, ProfileId, RequestIdentity, ServiceIdentity, VectorSpaceId,
};
pub use protocol::{
    ProtocolCapabilities, ProtocolOperation, ProtocolSearchRequest, ProtocolSearchResponse,
    DISKANN3_SERVICE_PROTOCOL_VERSION,
};
pub use replica::{
    ActiveGenerationSet, DeltaRecoveryContract, ImmutableReplicaSet, ReplicaEndpoint,
    ReplicaStorage,
};
pub use request::{AuthorizationContext, PredicatePlan, SearchRequest, TraceContext};
pub use response::{CompactSearchResult, CompletionState, SearchResponse, WorkTelemetry};
pub use router::{
    ShardDescriptor, ShardHealth, ShardManifest, ShardRoute, ShardRouteMetadata, ShardRouter,
    ShardRouterConfig,
};

/// Contract schema version for the DiskANN3 retrieval-service surface.
pub const DISKANN3_RETRIEVAL_SERVICE_SCHEMA_VERSION: u32 = 1;
