//! Fault-injection / compliance stubs for storage port contract tests.

use super::super::*;
use crate::task::{TaskId, TaskStatus};
use async_trait::async_trait;
use std::collections::BTreeSet;
use std::sync::Mutex;

pub(super) fn auth() -> StorageAuthContext {
    StorageAuthContext::new(StoragePrincipal::LocalAnonymous)
}

// ---------------------------------------------------------------------------
// Fault-injection / compliance stubs
// ---------------------------------------------------------------------------

/// In-memory facade that implements every port as an explicit unsupported or
/// fault-injectable stub. Used as the walking-skeleton trait-compliance surface.
#[derive(Debug, Default)]
pub(super) struct StubStorage {
    forced: Mutex<Option<StorageError>>,
    capabilities: BTreeSet<StorageCapabilityKind>,
    generation: Mutex<StorageGeneration>,
    manifests: Mutex<Vec<PublicationManifest>>,
}

impl StubStorage {
    pub(super) fn empty() -> Self {
        Self::default()
    }

    pub(super) fn with_all_capabilities() -> Self {
        Self {
            capabilities: StorageCapabilityKind::ALL.into_iter().collect(),
            ..Self::default()
        }
    }

    pub(super) fn force(&self, err: StorageError) {
        *self.forced.lock().expect("lock") = Some(err);
    }

    fn take_forced(&self) -> StorageResult<()> {
        match self.forced.lock().expect("lock").take() {
            Some(err) => Err(err),
            None => Ok(()),
        }
    }
}

impl StorageCapability for StubStorage {
    fn capability_descriptor(&self) -> StorageCapabilityDescriptor {
        StorageCapabilityDescriptor::new(self.capabilities.iter().copied())
            .with_backend_label("test_stub")
    }
}

#[async_trait]
impl CatalogStore for StubStorage {
    async fn list_sources(
        &self,
        _request: CatalogListSourcesRequest,
    ) -> StorageResult<CatalogListSourcesResponse> {
        self.require(StorageCapabilityKind::CatalogStore, "list_sources")?;
        self.take_forced()?;
        Ok(CatalogListSourcesResponse {
            page: PageResponse::empty(),
        })
    }

    async fn get_source(
        &self,
        request: CatalogGetSourceRequest,
    ) -> StorageResult<CatalogGetSourceResponse> {
        self.require(StorageCapabilityKind::CatalogStore, "get_source")?;
        self.take_forced()?;
        Err(StorageError::not_found("source", request.source_id.0))
    }

    async fn upsert_source(
        &self,
        request: CatalogUpsertSourceRequest,
    ) -> StorageResult<CatalogUpsertSourceResponse> {
        self.require(StorageCapabilityKind::CatalogStore, "upsert_source")?;
        self.take_forced()?;
        Ok(CatalogUpsertSourceResponse {
            source_id: request.source.id,
            generation: StorageGeneration::new(1),
        })
    }

    async fn list_collections(
        &self,
        _request: CatalogListCollectionsRequest,
    ) -> StorageResult<CatalogListCollectionsResponse> {
        self.require(StorageCapabilityKind::CatalogStore, "list_collections")?;
        self.take_forced()?;
        Ok(CatalogListCollectionsResponse {
            page: PageResponse::empty(),
        })
    }

    async fn get_collection(
        &self,
        request: CatalogGetCollectionRequest,
    ) -> StorageResult<CatalogGetCollectionResponse> {
        self.require(StorageCapabilityKind::CatalogStore, "get_collection")?;
        self.take_forced()?;
        Err(StorageError::not_found("collection", request.name))
    }

    async fn create_collection(
        &self,
        request: CatalogCreateCollectionRequest,
    ) -> StorageResult<CatalogCreateCollectionResponse> {
        self.require(StorageCapabilityKind::CatalogStore, "create_collection")?;
        self.take_forced()?;
        Ok(CatalogCreateCollectionResponse { name: request.name })
    }

    async fn delete_collection(
        &self,
        _request: CatalogDeleteCollectionRequest,
    ) -> StorageResult<CatalogDeleteCollectionResponse> {
        self.require(StorageCapabilityKind::CatalogStore, "delete_collection")?;
        self.take_forced()?;
        Ok(CatalogDeleteCollectionResponse { deleted: false })
    }

    async fn list_roots(
        &self,
        _request: CatalogListRootsRequest,
    ) -> StorageResult<CatalogListRootsResponse> {
        self.require(StorageCapabilityKind::CatalogStore, "list_roots")?;
        self.take_forced()?;
        Ok(CatalogListRootsResponse {
            page: PageResponse::empty(),
        })
    }

    async fn list_members(
        &self,
        _request: CatalogListMembersRequest,
    ) -> StorageResult<CatalogListMembersResponse> {
        self.require(StorageCapabilityKind::CatalogStore, "list_members")?;
        self.take_forced()?;
        Ok(CatalogListMembersResponse {
            page: PageResponse::empty(),
        })
    }
}

#[async_trait]
impl EvidenceStore for StubStorage {
    async fn list_evidence(
        &self,
        _request: EvidenceListRequest,
    ) -> StorageResult<EvidenceListResponse> {
        self.require(StorageCapabilityKind::EvidenceStore, "list_evidence")?;
        self.take_forced()?;
        Ok(EvidenceListResponse {
            page: PageResponse::empty(),
        })
    }

    async fn get_evidence(
        &self,
        request: EvidenceGetRequest,
    ) -> StorageResult<EvidenceGetResponse> {
        self.require(StorageCapabilityKind::EvidenceStore, "get_evidence")?;
        self.take_forced()?;
        Err(StorageError::not_found("evidence", request.evidence_id.0))
    }

    async fn put_evidence(
        &self,
        request: EvidencePutRequest,
    ) -> StorageResult<EvidencePutResponse> {
        self.require(StorageCapabilityKind::EvidenceStore, "put_evidence")?;
        self.take_forced()?;
        Ok(EvidencePutResponse {
            written: request.units.len() as u64,
            generation: StorageGeneration::new(1),
        })
    }

    async fn list_chunks(&self, _request: ChunkListRequest) -> StorageResult<ChunkListResponse> {
        self.require(StorageCapabilityKind::EvidenceStore, "list_chunks")?;
        self.take_forced()?;
        Ok(ChunkListResponse {
            page: PageResponse::empty(),
        })
    }

    async fn get_chunk(&self, request: ChunkGetRequest) -> StorageResult<ChunkGetResponse> {
        self.require(StorageCapabilityKind::EvidenceStore, "get_chunk")?;
        self.take_forced()?;
        Err(StorageError::not_found("chunk", request.chunk_id.0))
    }
}

#[async_trait]
impl BlobStore for StubStorage {
    async fn put_blob(&self, request: BlobPutRequest) -> StorageResult<BlobPutResponse> {
        self.require(StorageCapabilityKind::BlobStore, "put_blob")?;
        self.take_forced()?;
        Ok(BlobPutResponse {
            blob_id: request.blob_id,
            content_hash: "abc".into(),
            byte_len: request.bytes.len() as u64,
        })
    }

    async fn get_blob(&self, request: BlobGetRequest) -> StorageResult<BlobGetResponse> {
        self.require(StorageCapabilityKind::BlobStore, "get_blob")?;
        self.take_forced()?;
        Err(StorageError::not_found("blob", request.blob_id.0))
    }

    async fn head_blob(&self, request: BlobHeadRequest) -> StorageResult<BlobHeadResponse> {
        self.require(StorageCapabilityKind::BlobStore, "head_blob")?;
        self.take_forced()?;
        Err(StorageError::not_found("blob", request.blob_id.0))
    }

    async fn delete_blob(&self, _request: BlobDeleteRequest) -> StorageResult<BlobDeleteResponse> {
        self.require(StorageCapabilityKind::BlobStore, "delete_blob")?;
        self.take_forced()?;
        Ok(BlobDeleteResponse { deleted: false })
    }
}

#[async_trait]
impl TaskQueue for StubStorage {
    async fn enqueue(&self, _request: TaskEnqueueRequest) -> StorageResult<TaskEnqueueResponse> {
        self.require(StorageCapabilityKind::TaskQueue, "enqueue")?;
        self.take_forced()?;
        Ok(TaskEnqueueResponse {
            task_id: TaskId("task-1".into()),
            status: TaskStatus::Queued,
        })
    }

    async fn claim(&self, _request: TaskClaimRequest) -> StorageResult<TaskClaimResponse> {
        self.require(StorageCapabilityKind::TaskQueue, "claim")?;
        self.take_forced()?;
        Ok(TaskClaimResponse { tasks: Vec::new() })
    }

    async fn get_task(&self, request: TaskGetRequest) -> StorageResult<TaskGetResponse> {
        self.require(StorageCapabilityKind::TaskQueue, "get_task")?;
        self.take_forced()?;
        Err(StorageError::not_found("task", request.task_id.0))
    }

    async fn finish(&self, request: TaskFinishRequest) -> StorageResult<TaskFinishResponse> {
        self.require(StorageCapabilityKind::TaskQueue, "finish")?;
        self.take_forced()?;
        Ok(TaskFinishResponse {
            task_id: request.task_id,
            status: request.status,
        })
    }

    async fn list_tasks(&self, _request: TaskListRequest) -> StorageResult<TaskListResponse> {
        self.require(StorageCapabilityKind::TaskQueue, "list_tasks")?;
        self.take_forced()?;
        Ok(TaskListResponse {
            page: PageResponse::empty(),
        })
    }
}

#[async_trait]
impl LexicalSearch for StubStorage {
    async fn search(&self, _request: LexicalSearchRequest) -> StorageResult<LexicalSearchResponse> {
        self.require(StorageCapabilityKind::LexicalSearch, "search")?;
        self.take_forced()?;
        Ok(LexicalSearchResponse {
            page: PageResponse::empty(),
            generation: StorageGeneration::INITIAL,
        })
    }
}

#[async_trait]
impl VectorSearch for StubStorage {
    async fn search(&self, _request: VectorSearchRequest) -> StorageResult<VectorSearchResponse> {
        self.require(StorageCapabilityKind::VectorSearch, "search")?;
        self.take_forced()?;
        Ok(VectorSearchResponse {
            page: PageResponse::empty(),
            generation: StorageGeneration::INITIAL,
        })
    }
}

#[async_trait]
impl GraphSearch for StubStorage {
    async fn get_node(&self, request: GraphGetNodeRequest) -> StorageResult<GraphGetNodeResponse> {
        self.require(StorageCapabilityKind::GraphSearch, "get_node")?;
        self.take_forced()?;
        Err(StorageError::not_found("graph_node", request.node_id.0))
    }

    async fn neighbors(
        &self,
        _request: GraphNeighborsRequest,
    ) -> StorageResult<GraphNeighborsResponse> {
        self.require(StorageCapabilityKind::GraphSearch, "neighbors")?;
        self.take_forced()?;
        Ok(GraphNeighborsResponse {
            page: PageResponse::empty(),
        })
    }

    async fn get_edge(&self, request: GraphGetEdgeRequest) -> StorageResult<GraphGetEdgeResponse> {
        self.require(StorageCapabilityKind::GraphSearch, "get_edge")?;
        self.take_forced()?;
        Err(StorageError::not_found("graph_edge", request.edge_id.0))
    }
}

#[async_trait]
impl IndexPublisher for StubStorage {
    async fn publish(&self, request: IndexPublishRequest) -> StorageResult<IndexPublishResponse> {
        self.require(StorageCapabilityKind::IndexPublisher, "publish")?;
        self.take_forced()?;
        request.manifest.validate()?;
        let mut current = self.generation.lock().expect("lock");
        if let Some(expected) = request.expected_current {
            if *current != expected {
                return Err(StorageError::stale_generation(expected, *current));
            }
        }
        *current = request.manifest.generation;
        self.manifests
            .lock()
            .expect("lock")
            .push(request.manifest.clone());
        Ok(IndexPublishResponse {
            generation: request.manifest.generation,
            manifest: request.manifest,
        })
    }

    async fn current(&self, _request: IndexCurrentRequest) -> StorageResult<IndexCurrentResponse> {
        self.require(StorageCapabilityKind::IndexPublisher, "current")?;
        self.take_forced()?;
        let generation = *self.generation.lock().expect("lock");
        let manifest = self
            .manifests
            .lock()
            .expect("lock")
            .iter()
            .find(|m| m.generation == generation)
            .cloned();
        Ok(IndexCurrentResponse {
            generation,
            manifest,
        })
    }

    async fn get_manifest(
        &self,
        request: IndexGetManifestRequest,
    ) -> StorageResult<IndexGetManifestResponse> {
        self.require(StorageCapabilityKind::IndexPublisher, "get_manifest")?;
        self.take_forced()?;
        self.manifests
            .lock()
            .expect("lock")
            .iter()
            .find(|m| m.generation == request.generation)
            .cloned()
            .map(|manifest| IndexGetManifestResponse { manifest })
            .ok_or_else(|| {
                StorageError::not_found("publication_manifest", request.generation.to_string())
            })
    }
}

// ---------------------------------------------------------------------------
