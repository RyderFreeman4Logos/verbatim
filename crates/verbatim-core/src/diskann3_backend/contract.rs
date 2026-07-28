//! DiskANN3-specific extension contract for the repository's `VectorSearch` storage port.

use async_trait::async_trait;

use crate::storage_ports::VectorSearch;

use super::{
    BatchUpsertRequest, DiskAnnBackendResult, DiskAnnCapabilities, ExactVectorFetchRequest,
    ExactVectorFetchResponse, GenerationContext, GenerationReceipt, GenerationStatus,
    MutationReceipt, PageCacheDiagnostics, RangeSearchRequest, RestoreOrRebuildRequest, SearchPage,
    ShardGenerationRequest, ShutdownReceipt, SnapshotReceipt, TombstoneBatchRequest,
    TopKSearchRequest,
};

/// DiskANN3 lifecycle and exact-rescore operations layered over the core [`VectorSearch`] port.
///
/// A provider must also implement [`VectorSearch`], so ordinary dense search remains reachable
/// through the repository-standard storage capability while backend-specific operations stay
/// generation-, predicate-, and budget-bound at this adapter boundary.
#[async_trait]
pub trait DiskAnnVectorSearch: VectorSearch {
    /// Persist validated shard-generation inputs before a build may begin.
    async fn stage_shard_generation(
        &self,
        request: ShardGenerationRequest,
    ) -> DiskAnnBackendResult<GenerationReceipt>;

    /// Build staged shard data without publishing it.
    async fn build_shard_generation(
        &self,
        request: ShardGenerationRequest,
    ) -> DiskAnnBackendResult<GenerationReceipt>;

    /// Load a built shard generation into the provider's serving set.
    async fn load_shard_generation(
        &self,
        request: ShardGenerationRequest,
    ) -> DiskAnnBackendResult<GenerationReceipt>;

    /// Validate a loaded generation before publication.
    async fn validate_shard_generation(
        &self,
        request: ShardGenerationRequest,
    ) -> DiskAnnBackendResult<GenerationReceipt>;

    /// Perform an idempotent, mapping-validated batch upsert.
    async fn batch_upsert(
        &self,
        request: BatchUpsertRequest,
    ) -> DiskAnnBackendResult<MutationReceipt>;

    /// Perform an idempotent batch tombstone by stable vector ID.
    async fn tombstone(
        &self,
        request: TombstoneBatchRequest,
    ) -> DiskAnnBackendResult<MutationReceipt>;

    /// Execute a predicate-aware Top-K search.
    async fn top_k_search(&self, request: TopKSearchRequest) -> DiskAnnBackendResult<SearchPage>;

    /// Execute a predicate-aware raw-distance range search.
    async fn range_search(&self, request: RangeSearchRequest) -> DiskAnnBackendResult<SearchPage>;

    /// Fetch original full-precision vectors for final exact rescoring only.
    async fn fetch_exact_vectors(
        &self,
        request: ExactVectorFetchRequest,
    ) -> DiskAnnBackendResult<ExactVectorFetchResponse>;

    /// Report lifecycle status for one generation-bound context.
    async fn generation_status(
        &self,
        context: GenerationContext,
    ) -> DiskAnnBackendResult<GenerationStatus>;

    /// Discover bounded metric, quality, recovery, and resource capabilities.
    fn diskann_capabilities(&self) -> DiskAnnBackendResult<DiskAnnCapabilities>;

    /// Return bounded aggregate page-cache diagnostics for one generation context.
    async fn page_cache_diagnostics(
        &self,
        context: GenerationContext,
    ) -> DiskAnnBackendResult<PageCacheDiagnostics>;

    /// Snapshot one validated generation for a provider-independent restore path.
    async fn snapshot(&self, context: GenerationContext) -> DiskAnnBackendResult<SnapshotReceipt>;

    /// Restore a snapshot or reproducibly rebuild from authoritative original vectors.
    async fn restore_or_rebuild(
        &self,
        request: RestoreOrRebuildRequest,
    ) -> DiskAnnBackendResult<GenerationReceipt>;

    /// Shut down deterministically and attest all managed adapter resources were released.
    async fn shutdown(&self) -> DiskAnnBackendResult<ShutdownReceipt>;
}
