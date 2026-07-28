use async_trait::async_trait;
use verbatim_core::diskann3_backend::{
    BatchUpsertRequest, DiskAnnBackendResult, DiskAnnCapabilities, DiskAnnVectorSearch,
    ExactVectorFetchRequest, ExactVectorFetchResponse, GenerationContext, GenerationReceipt,
    GenerationStatus, MutationReceipt, PageCacheDiagnostics, RangeSearchRequest,
    RestoreOrRebuildRequest, SearchPage, ShardGenerationRequest, ShutdownReceipt, SnapshotReceipt,
    TombstoneBatchRequest, TopKSearchRequest,
};
use verbatim_core::storage_ports::{
    StorageCapability, StorageCapabilityDescriptor, StorageResult, VectorSearch, VectorSearchRequest,
    VectorSearchResponse,
};

struct ExternalProvider;

impl StorageCapability for ExternalProvider {
    fn capability_descriptor(&self) -> StorageCapabilityDescriptor {
        unimplemented!()
    }
}

#[async_trait]
impl VectorSearch for ExternalProvider {
    async fn search(&self, _request: VectorSearchRequest) -> StorageResult<VectorSearchResponse> {
        unimplemented!()
    }
}

#[async_trait]
impl DiskAnnVectorSearch for ExternalProvider {
    async fn stage_shard_generation(
        &self,
        _request: ShardGenerationRequest,
    ) -> DiskAnnBackendResult<GenerationReceipt> {
        unimplemented!()
    }

    async fn build_shard_generation(
        &self,
        _request: ShardGenerationRequest,
    ) -> DiskAnnBackendResult<GenerationReceipt> {
        unimplemented!()
    }

    async fn load_shard_generation(
        &self,
        _request: ShardGenerationRequest,
    ) -> DiskAnnBackendResult<GenerationReceipt> {
        unimplemented!()
    }

    async fn validate_shard_generation(
        &self,
        _request: ShardGenerationRequest,
    ) -> DiskAnnBackendResult<GenerationReceipt> {
        unimplemented!()
    }

    async fn batch_upsert(
        &self,
        _request: BatchUpsertRequest,
    ) -> DiskAnnBackendResult<MutationReceipt> {
        unimplemented!()
    }

    async fn tombstone(
        &self,
        _request: TombstoneBatchRequest,
    ) -> DiskAnnBackendResult<MutationReceipt> {
        unimplemented!()
    }

    async fn top_k_search(
        &self,
        _request: TopKSearchRequest,
    ) -> DiskAnnBackendResult<SearchPage> {
        unimplemented!()
    }

    async fn range_search(
        &self,
        _request: RangeSearchRequest,
    ) -> DiskAnnBackendResult<SearchPage> {
        unimplemented!()
    }

    async fn fetch_exact_vectors(
        &self,
        _request: ExactVectorFetchRequest,
    ) -> DiskAnnBackendResult<ExactVectorFetchResponse> {
        unimplemented!()
    }

    async fn generation_status(
        &self,
        _context: GenerationContext,
    ) -> DiskAnnBackendResult<GenerationStatus> {
        unimplemented!()
    }

    fn diskann_capabilities(&self) -> DiskAnnBackendResult<DiskAnnCapabilities> {
        unimplemented!()
    }

    async fn page_cache_diagnostics(
        &self,
        _context: GenerationContext,
    ) -> DiskAnnBackendResult<PageCacheDiagnostics> {
        unimplemented!()
    }

    async fn snapshot(&self, _context: GenerationContext) -> DiskAnnBackendResult<SnapshotReceipt> {
        unimplemented!()
    }

    async fn restore_or_rebuild(
        &self,
        _request: RestoreOrRebuildRequest,
    ) -> DiskAnnBackendResult<GenerationReceipt> {
        unimplemented!()
    }

    async fn shutdown(&self) -> DiskAnnBackendResult<ShutdownReceipt> {
        unimplemented!()
    }
}

fn main() {}
