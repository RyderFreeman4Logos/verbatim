use crate::diskann3::{PublicationGeneration, ShardId, VectorSpaceId};
use crate::diskann3_backend::{
    CandidateRepresentation, DiskAnnBackendDiagnosticCode, DiskAnnCapabilities,
    DiskAnnCapabilityFields, DiskAnnVectorSearch, FullQualityGuarantee, GenerationContext,
    IdempotencyKey, PageCacheDiagnosticFields, PageCacheDiagnostics, SearchBudget,
    SearchBudgetBinding, SearchBudgetFields, ShardGenerationRequest, StableVectorId,
    TombstoneBatchRequest, VectorMetric, VectorSpaceSpec,
};
use crate::types::EmbeddingProfileId;

fn search_budget() -> SearchBudget {
    SearchBudget::new(SearchBudgetFields {
        result_limit: 5,
        dense_candidate_limit: 10,
        lexical_candidate_limit: 10,
        exact_candidate_limit: 10,
        graph_candidate_limit: 10,
        fused_pool_limit: 10,
        rerank_candidate_limit: 10,
        full_precision_rescore_limit: 5,
        hydration_limit: 5,
        max_ssd_pages: 10,
        max_bytes_read: 1_024,
        max_cpu_micros: 1_024,
        max_work_units: 1_024,
        max_wall_time_micros: 1_024,
        max_concurrent_stages: 1,
        max_stage_attempts: 1,
        debug_record_limit: 5,
    })
    .expect("search budget")
}

fn generation_context() -> GenerationContext {
    let budget = search_budget();
    GenerationContext::new(
        VectorSpaceSpec::new(
            VectorSpaceId::new("text-default").expect("vector space"),
            EmbeddingProfileId::new("default").expect("embedding profile"),
            VectorMetric::L2,
        )
        .expect("vector space"),
        PublicationGeneration::new(7).expect("generation"),
        SearchBudgetBinding::new(budget, budget).expect("narrow budget binding"),
    )
    .expect("generation context")
}

fn capabilities() -> DiskAnnCapabilities {
    DiskAnnCapabilities::new(DiskAnnCapabilityFields {
        supported_metrics: vec![VectorMetric::Cosine, VectorMetric::Dot, VectorMetric::L2],
        supports_predicate_aware_search: true,
        supports_top_k: true,
        supports_range_search: true,
        supports_exact_vector_fetch: true,
        supports_batch_upsert: true,
        supports_tombstones: true,
        supports_snapshot_restore: true,
        supports_reproducible_rebuild: true,
        supports_deterministic_shutdown: true,
        max_page_reads: 10,
        max_cache_bytes: 1_024,
        max_bytes_read: 1_024,
        full_quality: FullQualityGuarantee::new(
            CandidateRepresentation::ProductQuantized,
            true,
            true,
        )
        .expect("full quality"),
    })
    .expect("capability envelope")
}

#[test]
fn diskann3_backend_exposes_capability_discovery_fields() {
    let capabilities = capabilities();
    let fields = capabilities.fields();

    assert!(fields.supports_predicate_aware_search);
    assert!(fields.supports_exact_vector_fetch);
    assert!(fields.supports_deterministic_shutdown);
    assert_eq!(fields.full_quality.original_dimension(), 4_096);
    assert_eq!(fields.max_cache_bytes, 1_024);
}

#[test]
fn diskann3_backend_rejects_unbounded_page_cache_diagnostics() {
    let context = generation_context();
    let fields = PageCacheDiagnosticFields {
        page_reads: 11,
        bytes_read: 1_024,
        cache_bytes: 1_024,
        cache_hits: 1,
        cache_misses: 1,
    };

    assert_eq!(
        PageCacheDiagnostics::new(fields, context.budget_binding(), &capabilities())
            .expect_err("diagnostics must remain bounded by the operation and adapter caps")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::PageCacheDiagnosticsExceeded
    );
}

#[test]
fn diskann3_backend_redacts_idempotency_diagnostics() {
    let error = IdempotencyKey::new("secret\ncustomer")
        .expect_err("control characters cannot enter an idempotency key");
    let rendered = format!("{error:?} {error}");

    assert_eq!(
        error.diagnostic_code(),
        DiskAnnBackendDiagnosticCode::InvalidIdempotencyKey
    );
    assert!(!rendered.contains("secret"));
    assert!(!rendered.contains("customer"));
}

#[test]
fn diskann3_backend_rejects_duplicate_tombstone_ids() {
    let context = generation_context();
    let vector_id = StableVectorId::new(11).expect("stable ID");
    let key = IdempotencyKey::new("delete-11").expect("idempotency key");

    assert_eq!(
        TombstoneBatchRequest::new(context, vec![vector_id, vector_id], key)
            .expect_err("a tombstone batch must not silently duplicate a stable vector ID")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::DuplicateMutationVectorId
    );
}

#[test]
fn diskann3_backend_rejects_mismatched_shard_generation() {
    let context = generation_context();
    let shard = ShardId::new(
        context.vector_space().vector_space_id().clone(),
        PublicationGeneration::new(8).expect("different generation"),
        0,
    )
    .expect("valid but mismatched shard identity");

    assert_eq!(
        ShardGenerationRequest::new(context, shard)
            .expect_err("shard generation must match the operation context")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::GenerationMismatch
    );
}

#[test]
fn diskann3_backend_contract_extends_vector_search() {
    #[allow(dead_code)]
    fn requires_vector_search<T: crate::storage_ports::VectorSearch>() {}
    #[allow(dead_code)]
    fn requires_adapter_contract<T: DiskAnnVectorSearch>() {
        requires_vector_search::<T>();
    }
}
