//! Narrow storage ports (DIST-004 / issue #350).
//!
//! Walking skeleton: capability-oriented trait definitions with typed
//! request/response, pagination, generation fencing, authorization context,
//! and fail-closed errors. No adapters and no daemon/retriever wiring.
//!
//! Residual: in-process SQLite/Tantivy/HNSW/Qdrant adapters, remove concrete
//! `Store` parameters from backend-neutral index/retrieval interfaces, shared
//! contract test harnesses for every adapter, closing #350. See
//! `docs/architecture/narrow-storage-ports.md`.

mod blob;
mod catalog;
mod common;
mod evidence;
mod publisher;
mod search;
mod task_queue;

pub use blob::{
    BlobDeleteRequest, BlobDeleteResponse, BlobGetRequest, BlobGetResponse, BlobHeadRequest,
    BlobHeadResponse, BlobId, BlobPutRequest, BlobPutResponse, BlobStore,
};
pub use catalog::{
    CatalogCreateCollectionRequest, CatalogCreateCollectionResponse,
    CatalogDeleteCollectionRequest, CatalogDeleteCollectionResponse, CatalogGetCollectionRequest,
    CatalogGetCollectionResponse, CatalogGetSourceRequest, CatalogGetSourceResponse,
    CatalogListCollectionsRequest, CatalogListCollectionsResponse, CatalogListMembersRequest,
    CatalogListMembersResponse, CatalogListRootsRequest, CatalogListRootsResponse,
    CatalogListSourcesRequest, CatalogListSourcesResponse, CatalogStore,
    CatalogUpsertSourceRequest, CatalogUpsertSourceResponse,
};
pub use common::{
    decode_auth_context_json, decode_capability_descriptor_json, decode_publication_manifest_json,
    DurationMillis, PageCursor, PageRequest, PageResponse, PublicationManifest, StorageAuthContext,
    StorageCapability, StorageCapabilityDescriptor, StorageCapabilityKind, StorageError,
    StorageGeneration, StoragePrincipal, StorageResult, STORAGE_PORTS_SCHEMA_VERSION,
};
pub use evidence::{
    ChunkGetRequest, ChunkGetResponse, ChunkListRequest, ChunkListResponse, EvidenceFilter,
    EvidenceGetRequest, EvidenceGetResponse, EvidenceListRequest, EvidenceListResponse,
    EvidencePutRequest, EvidencePutResponse, EvidenceStore,
};
pub use publisher::{
    IndexCurrentRequest, IndexCurrentResponse, IndexGetManifestRequest, IndexGetManifestResponse,
    IndexPublishRequest, IndexPublishResponse, IndexPublisher,
};
pub use search::{
    GraphGetEdgeRequest, GraphGetEdgeResponse, GraphGetNodeRequest, GraphGetNodeResponse,
    GraphNeighbor, GraphNeighborsRequest, GraphNeighborsResponse, GraphSearch, LexicalSearch,
    LexicalSearchHit, LexicalSearchRequest, LexicalSearchResponse, VectorSearch, VectorSearchHit,
    VectorSearchRequest, VectorSearchResponse,
};
pub use task_queue::{
    TaskClaimRequest, TaskClaimResponse, TaskEnqueueRequest, TaskEnqueueResponse,
    TaskFinishRequest, TaskFinishResponse, TaskGetRequest, TaskGetResponse, TaskListRequest,
    TaskListResponse, TaskQueue,
};

#[cfg(test)]
#[path = "../storage_ports_tests.rs"]
mod tests;
