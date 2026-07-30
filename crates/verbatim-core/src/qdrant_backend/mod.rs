//! Typed VECTOR-REF-001 Qdrant enterprise reference-backend adapter contract.
//!
//! This is a pure walking-skeleton contract: it has no Qdrant dependency or live
//! network implementation. The hand-written transitional REST adapter remains in
//! `crate::index::qdrant` until a separately validated official-client gRPC Query
//! API cutover. Qdrant is a reference backend, not the primary low-DRAM SSD ANN.

mod budget;
mod capability;
mod contract;
mod error;
mod filter;
mod grpc_path;
mod hydration;
mod identity;
mod lexical_caveat;
mod payload_index;
mod schema;
mod search_policy;

pub use budget::{BackpressureMarker, QdrantOperationBudget};
pub use capability::{QdrantCapabilities, QdrantCapabilityFields};
pub use contract::{
    QdrantMutationHook, QdrantSearchRequest, QdrantVectorSearch, QdrantVectorSearchSealed,
};
pub use error::{QdrantBackendDiagnosticCode, QdrantBackendError, QdrantBackendResult};
pub use filter::{FilterClause, FilterStrictness, QdrantFilterContract};
pub use grpc_path::{GrpcPathRequirements, QdrantQuerySurface, QdrantTransport};
pub use hydration::{HydrationRequest, QdrantPointRef};
pub use identity::{CollectionName, ConfigDigest, NamedVectorSpaceId, QdrantCollectionIdentity};
pub use lexical_caveat::{LexicalConformanceFlag, LexicalOwnership, QdrantLexicalPolicy};
pub use payload_index::{PayloadIndexKind, PayloadIndexPlan, PayloadIndexRequirement};
pub use schema::{
    QdrantCollectionSchema, QdrantSchemaFields, QdrantVectorMetric, QdrantVectorNormalization,
    QuantizationProfile,
};
pub use search_policy::{
    ForbiddenLocalPreSearch, LocalDenseParticipation, QdrantFailureReceipt, QdrantSearchPolicy,
    TypedQdrantFailure,
};

/// Contract schema version for the Qdrant reference adapter boundary.
pub const QDRANT_REFERENCE_BACKEND_ADAPTER_SCHEMA_VERSION: u32 = 1;
