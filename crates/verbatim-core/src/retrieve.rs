use std::collections::{HashMap, HashSet};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use anyhow::{anyhow, Context, Result};

mod source_filter;
mod vector_search_resource;

use crate::config::{GraphConfig, QdrantConfig, RerankConfig, RetrievalConfig};
#[cfg(feature = "qdrant")]
use crate::index::qdrant::{QdrantClient, QdrantHit};
use crate::provider::ProviderError;
use crate::resource::ObservableResource;
use crate::retrieval_telemetry::{CandidateCounters, SpanKind};
use crate::store::Store;
use crate::traits::{
    EmbeddingClient, LexicalIndex, RerankCapabilityState, RerankDiagnostics, RerankError,
    RerankRequestDiagnostics, Reranker, VectorIndex,
};
use crate::types::{
    Chunk, ChunkId, ChunkType, EdgeType, EmbeddingProfileId, EvidenceId, EvidenceKind,
    EvidenceUnit, GraphExpansionStep, GraphNodeId, GraphNodeKind, GraphTraversalDirection, ImageId,
    RetrievalDebug, RetrievalDebugEvidencePackMode, RetrievalDenseVectorPath,
    RetrievalEvidencePackEntry, RetrievalEvidenceRole, RetrievalFusedHit,
    RetrievalGraphExpansionDebug, RetrievalLocalSpansMs, RetrievalLocatorDebug,
    RetrievalProvenance, RetrievalRerankCapabilityDebug, RetrievalRerankCapabilityState,
    RetrievalRerankDebug, RetrievalRerankRequestDebug, RetrievalRerankScore, RetrievalRerankStatus,
    RetrievalResult, RetrievalStageHit, SourceId, SourceLocator, VectorIndexResidency,
};
use source_filter::{
    single_source_filter, source_filter_excludes, source_filter_ingest_hint, source_filter_scope,
};

const GRAPH_EXPANSION_SCORE_DECAY: f32 = 0.5;
const MAX_RERANK_CANDIDATE_CHUNKS: usize = 50;
const MAX_RERANK_DOCUMENT_CHARS: usize = 8_000;
static PREFIX_CACHE_BYPASS_COUNTER: AtomicU64 = AtomicU64::new(0);

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

pub struct RetrievalPipeline<'a> {
    vector_index: &'a dyn VectorIndex,
    lexical_index: &'a dyn LexicalIndex,
    store: &'a Store,
    embed_client: &'a dyn EmbeddingClient,
    embedding_enabled: bool,
    config: &'a RetrievalConfig,
    graph_config: Option<&'a GraphConfig>,
    rerank_config: Option<&'a RerankConfig>,
    reranker: Option<&'a dyn Reranker>,
    bypass_prefix_cache: bool,
    required_profile_id: Option<EmbeddingProfileId>,
    vector_residency: VectorIndexResidency,
    read_resource: Option<Arc<ObservableResource>>,
    vector_search_resource: Option<Arc<ObservableResource>>,
    #[cfg(feature = "qdrant")]
    qdrant: Option<QdrantClient>,
}

/// Visible display entries that should receive display-only support selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RetrievalDisplayScope {
    /// Run display-only support selection for every display entry.
    #[default]
    All,
    /// Run display-only support selection only for a zero-based display-entry range.
    Window { start: usize, len: usize },
}

impl RetrievalDisplayScope {
    /// Build a scope matching retrieve response pagination controls.
    pub fn page(limit: usize, page_size: usize, page: usize) -> Self {
        let start = page.saturating_sub(1).saturating_mul(page_size);
        let len = limit.saturating_sub(start).min(page_size);
        Self::Window { start, len }
    }

    fn contains(self, display_index: usize) -> bool {
        match self {
            Self::All => true,
            Self::Window { start, len } => {
                display_index >= start && display_index < start.saturating_add(len)
            }
        }
    }
}

/// Canonical debug selection budgets split by expensive support scoring and
/// compact display replacement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrievalCanonicalSelectionBudget {
    /// Canonical display entries whose support candidates may be embedded and scored.
    pub support: RetrievalDisplayScope,
    /// Canonical display entries allowed to replace first-evidence placeholders
    /// with the selected support evidence in compact display output.
    pub display: RetrievalDisplayScope,
}

impl RetrievalCanonicalSelectionBudget {
    /// Run canonical support and display selection for every display entry.
    pub fn all() -> Self {
        Self {
            support: RetrievalDisplayScope::All,
            display: RetrievalDisplayScope::All,
        }
    }

    /// Run both budgets over the same display-entry scope.
    pub fn scoped(scope: RetrievalDisplayScope) -> Self {
        Self {
            support: scope,
            display: scope,
        }
    }

    /// Build matching budgets from retrieve response pagination controls.
    pub fn page(limit: usize, page_size: usize, page: usize) -> Self {
        Self::scoped(RetrievalDisplayScope::page(limit, page_size, page))
    }

    /// Build independent support and display budgets.
    pub fn new(support: RetrievalDisplayScope, display: RetrievalDisplayScope) -> Self {
        Self { support, display }
    }
}

impl Default for RetrievalCanonicalSelectionBudget {
    fn default() -> Self {
        Self::all()
    }
}

/// Controls how much diagnostic evidence-pack data a debug search builds.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RetrievalDebugOptions {
    pub canonical_budget: RetrievalCanonicalSelectionBudget,
    pub evidence_pack_mode: RetrievalDebugEvidencePackMode,
}

impl RetrievalDebugOptions {
    pub fn full(canonical_budget: RetrievalCanonicalSelectionBudget) -> Self {
        Self {
            canonical_budget,
            evidence_pack_mode: RetrievalDebugEvidencePackMode::Full,
        }
    }

    pub fn compact(canonical_budget: RetrievalCanonicalSelectionBudget) -> Self {
        Self {
            canonical_budget,
            evidence_pack_mode: RetrievalDebugEvidencePackMode::Compact,
        }
    }
}

impl Default for RetrievalDebugOptions {
    fn default() -> Self {
        Self::full(RetrievalCanonicalSelectionBudget::default())
    }
}

/// Target used to derive detailed canonical support after ranking.
#[derive(Debug, Clone, Copy, PartialEq)]
pub enum RetrievalCanonicalDetailTarget<'a> {
    /// Select the canonical result containing this evidence id.
    EvidenceId(&'a EvidenceId),
    /// Select the canonical result containing this exact structured locator.
    Locator(&'a SourceLocator),
}

/// Detail payload for one canonical result without rerunning ranking.
#[derive(Debug, Clone, PartialEq)]
pub struct RetrievalCanonicalDetailDebug {
    /// Compact display entry selected for the target canonical result.
    pub display_entry: RetrievalEvidencePackEntry,
    /// Full support evidence entries for the target canonical result.
    pub support_evidence_pack: Vec<RetrievalEvidencePackEntry>,
}

#[cfg(feature = "qdrant")]
trait DenseHit {
    fn chunk_id(&self) -> ChunkId;
    fn score(&self) -> f32;
    fn profile_id(&self) -> Option<&EmbeddingProfileId> {
        None
    }
    fn source_id(&self) -> Option<&SourceId> {
        None
    }
    fn profile_generation(&self) -> Option<u64> {
        None
    }
}

#[cfg(feature = "qdrant")]
impl DenseHit for (ChunkId, f32) {
    fn chunk_id(&self) -> ChunkId {
        self.0.clone()
    }

    fn score(&self) -> f32 {
        self.1
    }
}

#[cfg(feature = "qdrant")]
impl DenseHit for QdrantHit {
    fn chunk_id(&self) -> ChunkId {
        self.chunk_id.clone()
    }

    fn score(&self) -> f32 {
        self.score
    }

    fn profile_id(&self) -> Option<&EmbeddingProfileId> {
        Some(&self.profile_id)
    }

    fn source_id(&self) -> Option<&SourceId> {
        Some(&self.source_id)
    }

    fn profile_generation(&self) -> Option<u64> {
        Some(self.profile_generation)
    }
}

impl<'a> RetrievalPipeline<'a> {
    pub fn new(
        vector_index: &'a dyn VectorIndex,
        lexical_index: &'a dyn LexicalIndex,
        store: &'a Store,
        embed_client: &'a dyn EmbeddingClient,
        config: &'a RetrievalConfig,
    ) -> Self {
        Self {
            vector_index,
            lexical_index,
            store,
            embed_client,
            embedding_enabled: true,
            config,
            graph_config: None,
            rerank_config: None,
            reranker: None,
            bypass_prefix_cache: false,
            required_profile_id: None,
            vector_residency: VectorIndexResidency::ResidentHnsw,
            read_resource: None,
            vector_search_resource: None,
            #[cfg(feature = "qdrant")]
            qdrant: None,
        }
    }

    pub fn new_with_graph(
        vector_index: &'a dyn VectorIndex,
        lexical_index: &'a dyn LexicalIndex,
        store: &'a Store,
        embed_client: &'a dyn EmbeddingClient,
        config: &'a RetrievalConfig,
        graph_config: &'a GraphConfig,
    ) -> Self {
        Self {
            vector_index,
            lexical_index,
            store,
            embed_client,
            embedding_enabled: true,
            config,
            graph_config: Some(graph_config),
            rerank_config: None,
            reranker: None,
            bypass_prefix_cache: false,
            required_profile_id: None,
            vector_residency: VectorIndexResidency::ResidentHnsw,
            read_resource: None,
            vector_search_resource: None,
            #[cfg(feature = "qdrant")]
            qdrant: None,
        }
    }

    pub fn require_embedding_profile(mut self, profile_id: &EmbeddingProfileId) -> Self {
        self.required_profile_id = Some(profile_id.clone());
        self
    }

    pub fn with_embedding_enabled(mut self, enabled: bool) -> Self {
        self.embedding_enabled = enabled;
        self
    }

    pub fn with_vector_residency(mut self, residency: VectorIndexResidency) -> Self {
        self.vector_residency = residency;
        self
    }

    pub fn with_read_resource(mut self, resource: Arc<ObservableResource>) -> Self {
        self.read_resource = Some(resource);
        self
    }

    pub fn with_reranker(mut self, config: &'a RerankConfig, reranker: &'a dyn Reranker) -> Self {
        self.rerank_config = Some(config);
        self.reranker = Some(reranker);
        self
    }

    pub fn with_prefix_cache_bypass(mut self, enabled: bool) -> Self {
        self.bypass_prefix_cache = enabled;
        self
    }

    fn with_read_permit<T>(&self, operation: impl FnOnce() -> Result<T>) -> Result<T> {
        let Some(resource) = &self.read_resource else {
            return operation();
        };
        let _permit = resource
            .acquire_blocking()
            .context("acquire sqlite reader resource for retrieval read")?;
        operation()
    }

    #[cfg(feature = "qdrant")]
    pub fn with_qdrant_search(mut self, config: &QdrantConfig) -> Self {
        if config.enabled && config.prefer_for_search {
            self.qdrant = QdrantClient::from_config(config);
        }
        self
    }

    #[cfg(not(feature = "qdrant"))]
    pub fn with_qdrant_search(self, _config: &QdrantConfig) -> Self {
        self
    }

    pub async fn search(&self, query: &str) -> Result<Vec<RetrievalResult>> {
        self.search_filtered(query, None).await
    }

    pub async fn search_filtered(
        &self,
        query: &str,
        source_filter: Option<&SourceId>,
    ) -> Result<Vec<RetrievalResult>> {
        let source_filter = source_filter.cloned().map(|source_id| {
            let mut source_ids = HashSet::new();
            source_ids.insert(source_id);
            source_ids
        });
        self.search_source_set(query, source_filter.as_ref()).await
    }

    pub async fn search_source_set(
        &self,
        query: &str,
        source_filter: Option<&HashSet<SourceId>>,
    ) -> Result<Vec<RetrievalResult>> {
        Ok(self
            .search_filtered_internal(query, source_filter, None)
            .await?
            .results)
    }

    pub async fn search_filtered_with_debug(
        &self,
        query: &str,
        source_filter: Option<&SourceId>,
    ) -> Result<(Vec<RetrievalResult>, RetrievalDebug)> {
        let source_filter = source_filter.cloned().map(|source_id| {
            let mut source_ids = HashSet::new();
            source_ids.insert(source_id);
            source_ids
        });
        self.search_source_set_with_debug(query, source_filter.as_ref())
            .await
    }

    pub async fn search_source_set_with_debug(
        &self,
        query: &str,
        source_filter: Option<&HashSet<SourceId>>,
    ) -> Result<(Vec<RetrievalResult>, RetrievalDebug)> {
        self.search_source_set_with_debug_canonical_budget(
            query,
            source_filter,
            RetrievalCanonicalSelectionBudget::all(),
        )
        .await
    }

    pub async fn search_source_set_with_debug_display_scope(
        &self,
        query: &str,
        source_filter: Option<&HashSet<SourceId>>,
        display_scope: RetrievalDisplayScope,
    ) -> Result<(Vec<RetrievalResult>, RetrievalDebug)> {
        self.search_source_set_with_debug_canonical_budget(
            query,
            source_filter,
            RetrievalCanonicalSelectionBudget::scoped(display_scope),
        )
        .await
    }

    pub async fn search_source_set_with_debug_canonical_budget(
        &self,
        query: &str,
        source_filter: Option<&HashSet<SourceId>>,
        canonical_budget: RetrievalCanonicalSelectionBudget,
    ) -> Result<(Vec<RetrievalResult>, RetrievalDebug)> {
        self.search_source_set_with_debug_options(
            query,
            source_filter,
            RetrievalDebugOptions::full(canonical_budget),
        )
        .await
    }

    pub async fn search_source_set_with_debug_options(
        &self,
        query: &str,
        source_filter: Option<&HashSet<SourceId>>,
        debug_options: RetrievalDebugOptions,
    ) -> Result<(Vec<RetrievalResult>, RetrievalDebug)> {
        self.search_filtered_internal(query, source_filter, Some(debug_options))
            .await?
            .into_results_with_debug()
    }

    async fn search_filtered_internal(
        &self,
        query: &str,
        source_filter: Option<&HashSet<SourceId>>,
        debug_options: Option<RetrievalDebugOptions>,
    ) -> Result<RetrievalSearchOutput> {
        let include_debug = debug_options.is_some();
        let debug_options = debug_options.unwrap_or_default();
        let mut local_spans_ms = RetrievalLocalSpansMs::default();
        let mut candidate_counters = CandidateCounters::default();
        if source_filter.is_some_and(HashSet::is_empty) {
            return Ok(empty_search_output(include_debug));
        }
        let setup_started = Instant::now();
        self.with_read_permit(|| {
            if self.embedding_enabled {
                self.ensure_required_profile_vectors(source_filter)?;
            }
            Ok(())
        })?;
        local_spans_ms.setup_ms = elapsed_ms(setup_started);
        let dense_top_k = if !self.embedding_enabled {
            0
        } else {
            self.config.dense_top_k
        };
        let bm25_top_k = self.config.bm25_top_k;
        candidate_counters.add_requested_k(SpanKind::DenseRetrieval, dense_top_k as u64)?;
        candidate_counters.add_requested_k(SpanKind::LexicalRetrieval, bm25_top_k as u64)?;

        let (dense_results, query_embedding_latency_ms, dense_vector_path, query_vector) =
            if self.embedding_enabled {
                let query_text = self.remote_query_text(self.embed_client.prepare_query(query));
                let embedding_started = Instant::now();
                let query_vec = self
                    .embed_client
                    .embed(&[query_text])
                    .await?
                    .into_iter()
                    .next()
                    .unwrap_or_default();
                let query_embedding_latency_ms = elapsed_ms(embedding_started);
                local_spans_ms.query_embedding_ms = query_embedding_latency_ms;
                let dense_started = Instant::now();
                let (dense_results, dense_vector_path) = self
                    .dense_search(
                        &query_vec,
                        dense_top_k,
                        source_filter,
                        &mut candidate_counters,
                        &mut local_spans_ms,
                    )
                    .await?;
                local_spans_ms.dense_vector_search_ms = elapsed_ms(dense_started);
                (
                    dense_results,
                    Some(query_embedding_latency_ms),
                    dense_vector_path,
                    Some(query_vec),
                )
            } else {
                (Vec::new(), None, RetrievalDenseVectorPath::Bm25Only, None)
            };
        candidate_counters.add_returned_k(SpanKind::DenseRetrieval, dense_results.len() as u64)?;
        candidate_counters.add_evaluated(dense_results.len() as u64)?;

        let (
            fused,
            bm25_hits,
            dense_hits,
            rrf_fused_hits,
            bm25_search_ms,
            rrf_fusion_ms,
            debug_candidate_pack_ms,
            bm25_returned,
            fused_count,
        ) = self.with_read_permit(|| {
            let bm25_started = Instant::now();
            let bm25_results = self.lexical_index.search(query, bm25_top_k)?;
            let bm25_search_ms = elapsed_ms(bm25_started);

            let rrf_started = Instant::now();
            let mut fused = rrf_fusion(&dense_results, &bm25_results, self.config.rrf_k);
            if source_filter.is_some() {
                let mut scoped_fused = Vec::with_capacity(fused.len());
                for candidate in fused {
                    let Some(chunk) = self.store.get_chunk(&candidate.0).ok().flatten() else {
                        continue;
                    };
                    if !source_filter_excludes(
                        source_filter,
                        &chunk.source_id,
                        &mut candidate_counters,
                    )? {
                        scoped_fused.push(candidate);
                    }
                }
                fused = scoped_fused;
            }
            let rrf_fusion_ms = elapsed_ms(rrf_started);
            let bm25_returned = bm25_results.len() as u64;
            let fused_count = fused.len() as u64;

            let debug_pack_started = Instant::now();
            let bm25_hits = if include_debug {
                self.stage_debug_hits(&bm25_results, source_filter)?
            } else {
                Vec::new()
            };
            let dense_hits = if include_debug {
                self.stage_debug_hits(&dense_results, source_filter)?
            } else {
                Vec::new()
            };
            let rrf_fused_hits = if include_debug {
                self.fused_debug_hits(&fused, &dense_results, &bm25_results)?
            } else {
                Vec::new()
            };
            let debug_candidate_pack_ms = elapsed_ms(debug_pack_started);
            Ok((
                fused,
                bm25_hits,
                dense_hits,
                rrf_fused_hits,
                bm25_search_ms,
                rrf_fusion_ms,
                debug_candidate_pack_ms,
                bm25_returned,
                fused_count,
            ))
        })?;
        candidate_counters.add_returned_k(SpanKind::LexicalRetrieval, bm25_returned)?;
        candidate_counters.add_evaluated(bm25_returned)?;
        candidate_counters.add_fused(fused_count)?;
        local_spans_ms.bm25_search_ms = bm25_search_ms;
        local_spans_ms.rrf_fusion_ms = rrf_fusion_ms;
        local_spans_ms.debug_candidate_pack_ms = debug_candidate_pack_ms;

        let rerank_started = Instant::now();
        let RerankOutcome {
            fused,
            debug: reranker_debug,
        } = self.rerank_fused(query, fused).await?;
        if matches!(reranker_debug.status, RetrievalRerankStatus::Succeeded) {
            candidate_counters.add_reranked(fused.len() as u64)?;
        }
        local_spans_ms.rerank_total_ms = elapsed_ms(rerank_started);

        let (results, graph_expanded_hits, result_hydration_ms, graph_expansion_ms, hydrated_count) =
            self.with_read_permit(|| {
                let result_hydration_started = Instant::now();
                let mut results = Vec::new();
                for (rank, (chunk_id, score)) in fused.into_iter().enumerate() {
                    let Some(chunk) = self.store.get_chunk(&chunk_id)? else {
                        continue;
                    };
                    let result_rank = rank + 1;
                    let provenance = RetrievalProvenance::seed(
                        result_rank,
                        chunk.id.clone(),
                        chunk.source_id.clone(),
                    );

                    results.push(self.result_for_chunk(chunk, score, provenance)?);
                }
                let result_hydration_ms = elapsed_ms(result_hydration_started);
                let hydrated_count = results.len() as u64;

                let graph_expansion_started = Instant::now();
                self.expand_graph_results(&mut results, source_filter, &mut candidate_counters)?;
                let graph_expansion_ms = elapsed_ms(graph_expansion_started);
                let graph_expanded_hits = if include_debug {
                    graph_expansion_debug_hits(&results)
                } else {
                    Vec::new()
                };
                Ok((
                    results,
                    graph_expanded_hits,
                    result_hydration_ms,
                    graph_expansion_ms,
                    hydrated_count,
                ))
            })?;
        candidate_counters.add_hydrated(hydrated_count)?;
        local_spans_ms.result_hydration_ms = result_hydration_ms;
        local_spans_ms.graph_expansion_ms = graph_expansion_ms;

        let mut final_evidence_count = 0usize;
        let final_evidence_pack = if include_debug
            && debug_options.evidence_pack_mode == RetrievalDebugEvidencePackMode::Full
        {
            let final_pack_started = Instant::now();
            let final_evidence_pack = final_evidence_pack_debug(&results);
            local_spans_ms.final_evidence_pack_ms = elapsed_ms(final_pack_started);
            final_evidence_count = final_evidence_pack.len();
            final_evidence_pack
        } else {
            if include_debug {
                final_evidence_count = final_evidence_debug_count(&results);
            }
            Vec::new()
        };

        let display_evidence_pack = if include_debug {
            let display_pack_started = Instant::now();
            let (
                display_evidence_pack,
                canonical_support_embedding_ms,
                canonical_display_selection_ms,
            ) = self
                .display_evidence_pack_debug(
                    query,
                    query_vector.as_deref(),
                    &results,
                    debug_options.canonical_budget,
                )
                .await;
            local_spans_ms.display_evidence_pack_ms = elapsed_ms(display_pack_started);
            local_spans_ms.canonical_support_embedding_ms = canonical_support_embedding_ms;
            local_spans_ms.canonical_display_selection_ms = canonical_display_selection_ms;
            display_evidence_pack
        } else {
            Vec::new()
        };

        let debug = if include_debug {
            let display_evidence_count = display_evidence_pack.len();
            Some(RetrievalDebug {
                dense_vector_path,
                query_embedding_latency_ms,
                retrieval_search_sql_statement_count: None,
                local_spans_ms,
                candidate_counters,
                evidence_pack_mode: debug_options.evidence_pack_mode,
                final_evidence_count,
                display_evidence_count,
                bm25_hits,
                dense_hits,
                rrf_fused_hits,
                graph_expanded_hits,
                reranker: reranker_debug,
                final_evidence_pack,
                display_evidence_pack,
            })
        } else {
            None
        };

        Ok(RetrievalSearchOutput { results, debug })
    }

    fn ensure_required_profile_vectors(
        &self,
        source_filter: Option<&HashSet<SourceId>>,
    ) -> Result<()> {
        let Some(profile_id) = &self.required_profile_id else {
            return Ok(());
        };
        let vector_count = match source_filter {
            Some(source_ids) => source_ids.iter().try_fold(0usize, |count, source_id| {
                self.store
                    .count_vector_documents_for_profile(profile_id, Some(source_id))
                    .map(|source_count| count + source_count)
            })?,
            None => self
                .store
                .count_vector_documents_for_profile(profile_id, None)?,
        };
        if vector_count > 0 {
            return Ok(());
        }
        let scope = source_filter_scope(source_filter);
        Err(anyhow!(
            "embedding profile '{}' has no vectors{scope}; build it with `verbatim ingest{} --embedding-profile {} --vectors-only` before asking, or request an explicit auto-build path when supported",
            profile_id,
            source_filter_ingest_hint(source_filter),
            profile_id,
        ))
    }

    fn local_dense_search(
        &self,
        query_vec: &[f32],
        top_k: usize,
        source_filter: Option<&HashSet<SourceId>>,
    ) -> Result<Vec<(ChunkId, f32)>> {
        if self.vector_residency == VectorIndexResidency::LowMemory {
            let default_profile_id;
            let profile_id = match &self.required_profile_id {
                Some(profile_id) => profile_id,
                None => {
                    default_profile_id = EmbeddingProfileId::default_profile();
                    &default_profile_id
                }
            };
            return self.store.search_vector_documents_for_profile(
                profile_id,
                query_vec,
                top_k,
                source_filter,
            );
        }
        let index_source_filter = single_source_filter(source_filter);
        Ok(self
            .vector_index
            .search_filtered(query_vec, top_k, index_source_filter))
    }

    fn dense_vector_path(&self) -> RetrievalDenseVectorPath {
        match self.vector_residency {
            VectorIndexResidency::LowMemory => RetrievalDenseVectorPath::LowMemorySqliteScan,
            VectorIndexResidency::ResidentHnsw => RetrievalDenseVectorPath::ResidentHnsw,
        }
    }

    #[cfg(feature = "qdrant")]
    fn valid_dense_hits<I>(
        &self,
        hits: I,
        top_k: usize,
        source_filter: Option<&HashSet<SourceId>>,
        required_profile: Option<(&EmbeddingProfileId, u64)>,
        candidate_counters: &mut CandidateCounters,
    ) -> Result<Vec<(ChunkId, f32)>>
    where
        I: IntoIterator,
        I::Item: DenseHit,
    {
        let mut valid = Vec::new();
        let mut seen = HashSet::new();
        let mut hits = hits.into_iter();
        while valid.len() < top_k
            && self.append_next_valid_dense_hit(
                &mut valid,
                &mut seen,
                &mut hits,
                source_filter,
                required_profile,
                candidate_counters,
            )?
        {}
        Ok(valid)
    }

    #[cfg(feature = "qdrant")]
    fn append_next_valid_dense_hit<I>(
        &self,
        target: &mut Vec<(ChunkId, f32)>,
        seen: &mut HashSet<ChunkId>,
        hits: &mut I,
        source_filter: Option<&HashSet<SourceId>>,
        required_profile: Option<(&EmbeddingProfileId, u64)>,
        candidate_counters: &mut CandidateCounters,
    ) -> Result<bool>
    where
        I: Iterator,
        I::Item: DenseHit,
    {
        for hit in hits {
            let chunk_id = hit.chunk_id();
            if seen.contains(&chunk_id) {
                continue;
            }
            let Some(chunk) = self.store.get_chunk(&chunk_id)? else {
                continue;
            };
            if source_filter_excludes(source_filter, &chunk.source_id, candidate_counters)? {
                continue;
            }
            if let Some((profile_id, profile_generation)) = required_profile {
                if hit.profile_id() != Some(profile_id)
                    || hit.source_id() != Some(&chunk.source_id)
                    || hit.profile_generation() != Some(profile_generation)
                {
                    continue;
                }
                if !self.store.has_vector_document_for_profile(
                    profile_id,
                    &chunk_id,
                    &chunk.source_id,
                )? {
                    continue;
                }
            }
            seen.insert(chunk_id.clone());
            let score = hit.score();
            target.push((chunk_id, score));
            return Ok(true);
        }

        Ok(false)
    }

    fn result_for_chunk(
        &self,
        chunk: Chunk,
        score: f32,
        provenance: RetrievalProvenance,
    ) -> Result<RetrievalResult> {
        let parent_chunk = if chunk.chunk_type == ChunkType::Child {
            chunk
                .parent_chunk_id
                .as_ref()
                .and_then(|pid| self.store.get_chunk(pid).ok().flatten())
        } else {
            None
        };

        let display_chunk = parent_chunk.unwrap_or_else(|| chunk.clone());
        let evidence_units = self.evidence_units_for_chunk(&chunk)?;

        Ok(RetrievalResult {
            chunk_id: chunk.id.clone(),
            score,
            chunk: display_chunk,
            evidence_units,
            provenance,
        })
    }

    async fn display_evidence_pack_debug(
        &self,
        query: &str,
        query_vector: Option<&[f32]>,
        results: &[RetrievalResult],
        canonical_budget: RetrievalCanonicalSelectionBudget,
    ) -> (Vec<RetrievalEvidencePackEntry>, Option<u64>, Option<u64>) {
        if !results.iter().any(canonical_multi_evidence_result) {
            return (final_evidence_pack_debug(results), None, None);
        }

        let support_results = canonical_scope_results(results, canonical_budget.support);
        let mut canonical_support_embedding_ms = None;
        let semantic_scores = match (query_vector, support_results.is_empty()) {
            (_, true) => HashMap::new(),
            (Some(query_vector), false) => {
                let semantic_started = Instant::now();
                let semantic_scores = self
                    .canonical_support_semantic_scores(query_vector, &support_results)
                    .await;
                canonical_support_embedding_ms = Some(elapsed_ms(semantic_started));
                semantic_scores
            }
            (None, false) => HashMap::new(),
        };
        let selection_started = Instant::now();
        let support_ids = canonical_display_support_ids(query, &support_results, &semantic_scores);
        let display_pack = display_evidence_pack_debug_with_canonical_selection(
            results,
            &support_ids,
            canonical_budget.display,
        );
        (
            display_pack,
            canonical_support_embedding_ms,
            Some(elapsed_ms(selection_started)),
        )
    }

    /// Build detailed canonical support/display debug data for one target
    /// evidence id or locator from already ranked retrieval results.
    pub async fn canonical_detail_evidence_pack_debug(
        &self,
        query: &str,
        results: &[RetrievalResult],
        target: RetrievalCanonicalDetailTarget<'_>,
    ) -> Option<RetrievalCanonicalDetailDebug> {
        let result = canonical_detail_result(results, target)?;
        let query_vector = self.canonical_detail_query_vector(query).await;
        let detail_results = [result];
        let semantic_scores = match query_vector.as_deref() {
            Some(query_vector) => {
                self.canonical_support_semantic_scores(query_vector, &detail_results)
                    .await
            }
            None => HashMap::new(),
        };
        let support_ids = canonical_display_support_ids(query, &detail_results, &semantic_scores);
        let display_entry = display_evidence_pack_debug_with_canonical_selection(
            std::slice::from_ref(result),
            &support_ids,
            RetrievalDisplayScope::All,
        )
        .into_iter()
        .next()?;
        let support_evidence_pack = final_evidence_pack_debug(std::slice::from_ref(result));

        Some(RetrievalCanonicalDetailDebug {
            display_entry,
            support_evidence_pack,
        })
    }

    async fn canonical_detail_query_vector(&self, query: &str) -> Option<Vec<f32>> {
        if !self.embedding_enabled {
            return None;
        }

        let query_text = self.remote_query_text(self.embed_client.prepare_query(query));
        match self.embed_client.embed(&[query_text]).await {
            Ok(vectors) => vectors.into_iter().next(),
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "canonical detail query embedding failed; falling back to lexical support selection"
                );
                None
            }
        }
    }

    async fn canonical_support_semantic_scores(
        &self,
        query_vector: &[f32],
        results: &[&RetrievalResult],
    ) -> HashMap<String, f32> {
        let mut candidates = Vec::new();
        let mut documents = Vec::new();

        for result in results
            .iter()
            .copied()
            .filter(|result| canonical_multi_evidence_result(result))
        {
            for evidence in &result.evidence_units {
                candidates.push(evidence.id.0.clone());
                documents.push(
                    self.embed_client
                        .prepare_document(&evidence.text, &evidence.heading_path.join(" / ")),
                );
            }
        }

        if documents.is_empty() {
            return HashMap::new();
        }

        let vectors = match self.embed_client.embed(&documents).await {
            Ok(vectors) => vectors,
            Err(error) => {
                tracing::warn!(
                    error = %error,
                    "canonical support evidence embedding failed; falling back to lexical display selection"
                );
                return HashMap::new();
            }
        };

        candidates
            .into_iter()
            .zip(vectors)
            .filter_map(|(evidence_id, vector)| {
                cosine_similarity(query_vector, &vector)
                    .filter(|score| score.is_finite())
                    .map(|score| (evidence_id, score))
            })
            .collect()
    }

    async fn rerank_fused(&self, query: &str, fused: Vec<(ChunkId, f32)>) -> Result<RerankOutcome> {
        let Some(config) = self.rerank_config else {
            return Ok(RerankOutcome {
                fused,
                debug: RetrievalRerankDebug::disabled(),
            });
        };
        if !config.enabled {
            return Ok(RerankOutcome {
                fused,
                debug: RetrievalRerankDebug::disabled(),
            });
        }
        if fused.is_empty() {
            return Ok(RerankOutcome {
                fused,
                debug: RetrievalRerankDebug::skipped("no_candidates"),
            });
        }

        let Some(reranker) = self.reranker else {
            return Ok(RerankOutcome {
                fused,
                debug: RetrievalRerankDebug::skipped("reranker_not_configured"),
            });
        };

        let candidates = self.with_read_permit(|| {
            let mut candidates = Vec::new();
            for (chunk_id, _) in fused.iter().take(MAX_RERANK_CANDIDATE_CHUNKS) {
                let Some(chunk) = self.store.get_chunk(chunk_id)? else {
                    continue;
                };
                candidates.push(RerankCandidate {
                    chunk_id: chunk_id.clone(),
                    text: rerank_document_text(&chunk),
                });
            }
            Ok(candidates)
        })?;

        if candidates.is_empty() {
            return Ok(RerankOutcome {
                fused,
                debug: RetrievalRerankDebug::skipped("no_available_candidate_text"),
            });
        }

        let top_n = bounded_rerank_top_n(config.top_n, candidates.len());
        let documents = candidates
            .iter()
            .map(|candidate| candidate.text.clone())
            .collect::<Vec<_>>();

        let rerank_started = Instant::now();
        let query = self.remote_query_text(query.to_string());
        match reranker
            .rerank_with_diagnostics(&query, &documents, top_n)
            .await
        {
            Ok(response) => {
                let latency_ms = elapsed_ms(rerank_started);
                let actual_top_n =
                    actual_rerank_top_n(top_n, response.diagnostics.request.as_ref());
                let actual_candidate_count = actual_rerank_candidate_count(
                    candidates.len(),
                    response.diagnostics.request.as_ref(),
                );
                let (reranked, scores) = validated_rerank_hits(
                    &candidates,
                    response.hits,
                    actual_top_n,
                    actual_candidate_count,
                );
                if reranked.is_empty() {
                    return Ok(RerankOutcome {
                        fused,
                        debug: with_rerank_diagnostics(
                            RetrievalRerankDebug::fallback(
                                &config.provider,
                                &config.model,
                                actual_top_n,
                                actual_candidate_count,
                                "no_usable_results",
                            )
                            .with_latency_ms(latency_ms),
                            Some(&response.diagnostics),
                        ),
                    });
                }
                Ok(RerankOutcome {
                    fused: reranked,
                    debug: with_rerank_diagnostics(
                        RetrievalRerankDebug::succeeded(
                            &config.provider,
                            &config.model,
                            actual_top_n,
                            actual_candidate_count,
                            scores,
                        )
                        .with_latency_ms(latency_ms),
                        Some(&response.diagnostics),
                    ),
                })
            }
            Err(error) => {
                let latency_ms = elapsed_ms(rerank_started);
                let (reason, diagnostics) = rerank_error_reason_and_diagnostics(&error);
                let actual_top_n = diagnostics
                    .and_then(|diagnostics| diagnostics.request.as_ref())
                    .map_or(top_n, |request| actual_rerank_top_n(top_n, Some(request)));
                let actual_candidate_count = diagnostics
                    .and_then(|diagnostics| diagnostics.request.as_ref())
                    .map_or(candidates.len(), |request| {
                        actual_rerank_candidate_count(candidates.len(), Some(request))
                    });
                tracing::warn!(reason = %reason, "rerank failed; falling back to RRF ordering");
                Ok(RerankOutcome {
                    fused,
                    debug: with_rerank_diagnostics(
                        RetrievalRerankDebug::fallback(
                            &config.provider,
                            &config.model,
                            actual_top_n,
                            actual_candidate_count,
                            reason,
                        )
                        .with_latency_ms(latency_ms),
                        diagnostics,
                    ),
                })
            }
        }
    }

    fn evidence_units_for_chunk(&self, chunk: &Chunk) -> Result<Vec<EvidenceUnit>> {
        let mut evidence_units = Vec::new();
        let mut seen = HashSet::new();

        for evidence_id in &chunk.evidence_unit_ids {
            let Some(unit) = self.store.get_evidence(evidence_id)? else {
                continue;
            };
            let derived_from = unit.derived_from.clone();
            push_unique_evidence(&mut evidence_units, &mut seen, unit);

            if let Some(source_evidence_id) = derived_from {
                if let Some(source_unit) = self.store.get_evidence(&source_evidence_id)? {
                    push_unique_evidence(&mut evidence_units, &mut seen, source_unit);
                }
            }
        }

        Ok(evidence_units)
    }

    fn stage_debug_hits(
        &self,
        hits: &[(ChunkId, f32)],
        source_filter: Option<&HashSet<SourceId>>,
    ) -> Result<Vec<RetrievalStageHit>> {
        let mut debug_hits = Vec::new();

        for (rank, (chunk_id, score)) in hits.iter().enumerate() {
            let chunk = self.store.get_chunk(chunk_id)?;
            if source_filter.is_some_and(|source_ids| {
                chunk
                    .as_ref()
                    .is_none_or(|chunk| !source_ids.contains(&chunk.source_id))
            }) {
                continue;
            }

            debug_hits.push(RetrievalStageHit {
                rank: rank + 1,
                chunk_id: chunk_id.clone(),
                source_id: chunk.as_ref().map(|chunk| chunk.source_id.clone()),
                score: *score,
                evidence_ids: chunk
                    .map(|chunk| chunk.evidence_unit_ids)
                    .unwrap_or_default(),
            });
        }

        Ok(debug_hits)
    }

    fn fused_debug_hits(
        &self,
        hits: &[(ChunkId, f32)],
        dense_results: &[(ChunkId, f32)],
        bm25_results: &[(ChunkId, f32)],
    ) -> Result<Vec<RetrievalFusedHit>> {
        let dense_ranks = rank_by_chunk_id(dense_results);
        let bm25_ranks = rank_by_chunk_id(bm25_results);

        hits.iter()
            .enumerate()
            .map(|(rank, (chunk_id, score))| {
                let chunk = self.store.get_chunk(chunk_id)?;
                Ok(RetrievalFusedHit {
                    rank: rank + 1,
                    chunk_id: chunk_id.clone(),
                    source_id: chunk.as_ref().map(|chunk| chunk.source_id.clone()),
                    score: *score,
                    dense_rank: dense_ranks.get(chunk_id).copied(),
                    bm25_rank: bm25_ranks.get(chunk_id).copied(),
                    evidence_ids: chunk
                        .map(|chunk| chunk.evidence_unit_ids)
                        .unwrap_or_default(),
                })
            })
            .collect()
    }

    fn expand_graph_results(
        &self,
        results: &mut Vec<RetrievalResult>,
        source_filter: Option<&HashSet<SourceId>>,
        candidate_counters: &mut CandidateCounters,
    ) -> Result<()> {
        let Some(config) = self.graph_config else {
            return Ok(());
        };
        if !config.enabled
            || config.max_hops == 0
            || config.max_expanded_chunks == 0
            || config.max_neighbors_per_seed == 0
            || config.edge_types.is_empty()
        {
            return Ok(());
        }

        let mut edge_types = config.edge_types.clone();
        edge_types.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        edge_types.dedup();
        let seeds = results.clone();
        let mut seen_chunks = results
            .iter()
            .map(|result| result.chunk_id.0.clone())
            .collect::<HashSet<_>>();
        let mut expanded_count = 0usize;

        for seed in &seeds {
            if expanded_count >= config.max_expanded_chunks {
                break;
            }

            let seed_rank = seed
                .provenance
                .seed_rank
                .unwrap_or(seed.provenance.result_rank)
                .max(1);
            let mut state = GraphExpansionState {
                config,
                edge_types: &edge_types,
                results,
                seen_chunks: &mut seen_chunks,
                expanded_count: &mut expanded_count,
                seed_expanded_count: 0,
                source_filter,
                candidate_counters,
            };

            self.expand_page_images(seed, seed_rank, &mut state)?;
            self.expand_section_contains(seed, seed_rank, &mut state)?;
            self.expand_graph_bfs(seed, seed_rank, &mut state)?;
        }

        Ok(())
    }

    fn expand_page_images(
        &self,
        seed: &RetrievalResult,
        seed_rank: usize,
        state: &mut GraphExpansionState<'_>,
    ) -> Result<()> {
        if !state.edge_types.contains(&EdgeType::PageContainsImage) {
            return Ok(());
        }

        let mut page_node_ids = Vec::new();
        let mut seen_pages = HashSet::new();
        for evidence in &seed.evidence_units {
            let Some(page) = locator_page(&evidence.locator) else {
                continue;
            };
            if seen_pages.insert((evidence.source_id.0.clone(), page)) {
                page_node_ids.push(GraphNodeId::new(
                    &evidence.source_id,
                    GraphNodeKind::Page,
                    &format!("page:{page}"),
                ));
            }
        }

        for page_node_id in page_node_ids {
            if !state.has_budget() {
                break;
            }

            let limit = state.remaining_candidate_limit();
            let mut edges = self.store.list_graph_edges_from_by_types_limited(
                &page_node_id,
                &[EdgeType::PageContainsImage],
                limit,
            )?;
            sort_edges(&mut edges);

            for edge in edges {
                if !state.has_budget() {
                    break;
                }
                let path = vec![graph_step(&edge, GraphTraversalDirection::Outgoing)];
                self.try_push_graph_candidate(
                    seed,
                    seed_rank,
                    CandidateExpansion {
                        node_id: &edge.to_node_id,
                        score: derived_graph_score(seed.score, 1),
                        hop_distance: 1,
                        graph_path: path,
                    },
                    state,
                )?;
            }
        }

        Ok(())
    }

    fn expand_section_contains(
        &self,
        seed: &RetrievalResult,
        seed_rank: usize,
        state: &mut GraphExpansionState<'_>,
    ) -> Result<()> {
        if !state.edge_types.contains(&EdgeType::SectionContains) {
            return Ok(());
        }

        let seed_chunk_node_id = GraphNodeId::new(
            &seed.chunk.source_id,
            GraphNodeKind::Chunk,
            &seed.chunk_id.0,
        );
        let limit = state.remaining_candidate_limit();
        let mut containing_edges = self.store.list_graph_edges_to_by_types_limited(
            &seed_chunk_node_id,
            &[EdgeType::SectionContains],
            limit,
        )?;
        sort_edges(&mut containing_edges);

        for containing_edge in containing_edges {
            if !state.has_budget() {
                break;
            }

            let section_node_id = containing_edge.from_node_id.clone();
            let limit = state.remaining_candidate_limit();
            let mut contained_edges = self.store.list_graph_edges_from_by_types_limited(
                &section_node_id,
                &[EdgeType::SectionContains],
                limit,
            )?;
            sort_edges(&mut contained_edges);

            for edge in contained_edges {
                if !state.has_budget() {
                    break;
                }
                let path = vec![
                    graph_step(&containing_edge, GraphTraversalDirection::Incoming),
                    graph_step(&edge, GraphTraversalDirection::Outgoing),
                ];
                self.try_push_graph_candidate(
                    seed,
                    seed_rank,
                    CandidateExpansion {
                        node_id: &edge.to_node_id,
                        score: derived_graph_score(seed.score, 1),
                        hop_distance: 1,
                        graph_path: path,
                    },
                    state,
                )?;
            }
        }

        Ok(())
    }

    fn expand_graph_bfs(
        &self,
        seed: &RetrievalResult,
        seed_rank: usize,
        state: &mut GraphExpansionState<'_>,
    ) -> Result<()> {
        let seed_nodes = self.seed_graph_nodes(seed)?;
        let mut visited_nodes = seed_nodes
            .iter()
            .map(|node_id| node_id.0.clone())
            .collect::<HashSet<_>>();
        let mut frontier = seed_nodes
            .into_iter()
            .map(|node_id| FrontierNode {
                node_id,
                path: Vec::new(),
            })
            .collect::<Vec<_>>();

        for hop in 1..=state.config.max_hops {
            if frontier.is_empty() || !state.has_budget() {
                break;
            }

            let mut next_frontier = Vec::new();
            for frontier_node in frontier {
                if !state.has_budget() {
                    break;
                }
                if self.is_source_graph_node(&frontier_node.node_id)? {
                    continue;
                }

                let limit = state.remaining_candidate_limit();
                for transition in
                    self.graph_transitions(&frontier_node.node_id, state.edge_types, limit)?
                {
                    if !state.has_budget() {
                        break;
                    }

                    let mut path = frontier_node.path.clone();
                    path.push(transition.step);

                    if visited_nodes.insert(transition.neighbor_node_id.0.clone()) {
                        next_frontier.push(FrontierNode {
                            node_id: transition.neighbor_node_id.clone(),
                            path: path.clone(),
                        });
                    }

                    self.try_push_graph_candidate(
                        seed,
                        seed_rank,
                        CandidateExpansion {
                            node_id: &transition.neighbor_node_id,
                            score: derived_graph_score(seed.score, hop),
                            hop_distance: hop as u32,
                            graph_path: path,
                        },
                        state,
                    )?;
                }
            }

            frontier = next_frontier;
        }

        Ok(())
    }

    fn seed_graph_nodes(&self, seed: &RetrievalResult) -> Result<Vec<GraphNodeId>> {
        let mut node_ids = Vec::new();
        let mut seen = HashSet::new();

        push_unique_node_id(
            &mut node_ids,
            &mut seen,
            GraphNodeId::new(
                &seed.chunk.source_id,
                GraphNodeKind::Chunk,
                &seed.chunk_id.0,
            ),
        );

        for evidence in &seed.evidence_units {
            push_unique_node_id(
                &mut node_ids,
                &mut seen,
                GraphNodeId::new(
                    &evidence.source_id,
                    GraphNodeKind::EvidenceUnit,
                    &evidence.id.0,
                ),
            );

            if let Some(artifact) = self.store.get_image_artifact_by_evidence(&evidence.id)? {
                push_unique_node_id(
                    &mut node_ids,
                    &mut seen,
                    GraphNodeId::new(
                        &artifact.source_id,
                        GraphNodeKind::ImageArtifact,
                        &artifact.image_id.0,
                    ),
                );
            }
        }

        Ok(node_ids)
    }

    fn graph_transitions(
        &self,
        node_id: &GraphNodeId,
        edge_types: &[EdgeType],
        limit: usize,
    ) -> Result<Vec<GraphTransition>> {
        let mut transitions = Vec::new();

        for edge in self
            .store
            .list_graph_edges_from_by_types_limited(node_id, edge_types, limit)?
        {
            transitions.push(GraphTransition {
                neighbor_node_id: edge.to_node_id.clone(),
                step: graph_step(&edge, GraphTraversalDirection::Outgoing),
            });
        }

        let remaining_limit = limit.saturating_sub(transitions.len());
        for edge in
            self.store
                .list_graph_edges_to_by_types_limited(node_id, edge_types, remaining_limit)?
        {
            transitions.push(GraphTransition {
                neighbor_node_id: edge.from_node_id.clone(),
                step: graph_step(&edge, GraphTraversalDirection::Incoming),
            });
        }

        transitions.sort_by(|left, right| {
            left.step
                .edge_type
                .as_str()
                .cmp(right.step.edge_type.as_str())
                .then_with(|| left.step.from_node_id.0.cmp(&right.step.from_node_id.0))
                .then_with(|| left.step.to_node_id.0.cmp(&right.step.to_node_id.0))
                .then_with(|| {
                    direction_key(&left.step.direction).cmp(direction_key(&right.step.direction))
                })
        });

        Ok(transitions)
    }

    fn is_source_graph_node(&self, node_id: &GraphNodeId) -> Result<bool> {
        Ok(self
            .store
            .get_graph_node(node_id)?
            .is_some_and(|node| node.kind == GraphNodeKind::Source))
    }

    fn try_push_graph_candidate(
        &self,
        seed: &RetrievalResult,
        seed_rank: usize,
        candidate: CandidateExpansion<'_>,
        state: &mut GraphExpansionState<'_>,
    ) -> Result<()> {
        if !state.has_budget() {
            return Ok(());
        }

        let provenance = RetrievalProvenance::graph_expansion(
            state.results.len() + 1,
            seed_rank,
            seed.chunk_id.clone(),
            seed.chunk.source_id.clone(),
            candidate.hop_distance,
            candidate.graph_path,
        );
        let Some(result) =
            self.result_for_graph_node(candidate.node_id, candidate.score, provenance)?
        else {
            return Ok(());
        };
        if source_filter_excludes(
            state.source_filter,
            &result.chunk.source_id,
            state.candidate_counters,
        )? {
            return Ok(());
        }
        if !state.seen_chunks.insert(result.chunk_id.0.clone()) {
            return Ok(());
        }

        state.results.push(result);
        *state.expanded_count += 1;
        state.seed_expanded_count += 1;
        Ok(())
    }

    fn result_for_graph_node(
        &self,
        node_id: &GraphNodeId,
        score: f32,
        provenance: RetrievalProvenance,
    ) -> Result<Option<RetrievalResult>> {
        let Some(node) = self.store.get_graph_node(node_id)? else {
            return Ok(None);
        };

        match node.kind {
            GraphNodeKind::Chunk => {
                self.result_for_chunk_id(ChunkId(node.external_id), score, provenance)
            }
            GraphNodeKind::EvidenceUnit => {
                self.result_for_evidence_id(EvidenceId(node.external_id), score, provenance)
            }
            GraphNodeKind::ImageArtifact => {
                let Some(artifact) = self.store.get_image_artifact(&ImageId(node.external_id))?
                else {
                    return Ok(None);
                };
                self.result_for_evidence_id(artifact.evidence_id, score, provenance)
            }
            GraphNodeKind::Source
            | GraphNodeKind::Page
            | GraphNodeKind::Section
            | GraphNodeKind::GeneratedEntity
            | GraphNodeKind::GeneratedClaim => Ok(None),
        }
    }

    fn result_for_chunk_id(
        &self,
        chunk_id: ChunkId,
        score: f32,
        provenance: RetrievalProvenance,
    ) -> Result<Option<RetrievalResult>> {
        let Some(chunk) = self.store.get_chunk(&chunk_id)? else {
            return Ok(None);
        };
        Ok(Some(self.result_for_chunk(chunk, score, provenance)?))
    }

    fn result_for_evidence_id(
        &self,
        evidence_id: EvidenceId,
        score: f32,
        provenance: RetrievalProvenance,
    ) -> Result<Option<RetrievalResult>> {
        let Some(chunk) = self
            .store
            .list_chunks_for_evidence(&evidence_id)?
            .into_iter()
            .next()
        else {
            return Ok(None);
        };
        Ok(Some(self.result_for_chunk(chunk, score, provenance)?))
    }

    fn remote_query_text(&self, query: String) -> String {
        if self.bypass_prefix_cache {
            prefix_cache_bypass_query(query)
        } else {
            query
        }
    }
}

fn prefix_cache_bypass_query(query: String) -> String {
    let nonce = prefix_cache_bypass_nonce();
    let mut prefixed = String::with_capacity(query.len() + 66);
    prefixed.push('\n');
    for bit in 0..64 {
        if (nonce >> bit) & 1 == 0 {
            prefixed.push(' ');
        } else {
            prefixed.push('\t');
        }
    }
    prefixed.push('\n');
    prefixed.push_str(&query);
    prefixed
}

fn prefix_cache_bypass_nonce() -> u64 {
    let counter = PREFIX_CACHE_BYPASS_COUNTER.fetch_add(1, Ordering::Relaxed);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_nanos() as u64);
    nanos ^ counter.rotate_left(17) ^ u64::from(std::process::id())
}

struct RetrievalSearchOutput {
    results: Vec<RetrievalResult>,
    debug: Option<RetrievalDebug>,
}

struct RerankOutcome {
    fused: Vec<(ChunkId, f32)>,
    debug: RetrievalRerankDebug,
}

struct RerankCandidate {
    chunk_id: ChunkId,
    text: String,
}

impl RetrievalSearchOutput {
    fn into_results_with_debug(self) -> Result<(Vec<RetrievalResult>, RetrievalDebug)> {
        let debug = self
            .debug
            .ok_or_else(|| anyhow!("retrieval debug output missing after debug search"))?;
        Ok((self.results, debug))
    }
}

struct GraphExpansionState<'a> {
    config: &'a GraphConfig,
    edge_types: &'a [EdgeType],
    results: &'a mut Vec<RetrievalResult>,
    seen_chunks: &'a mut HashSet<String>,
    expanded_count: &'a mut usize,
    seed_expanded_count: usize,
    source_filter: Option<&'a HashSet<SourceId>>,
    candidate_counters: &'a mut CandidateCounters,
}

impl GraphExpansionState<'_> {
    fn has_budget(&self) -> bool {
        *self.expanded_count < self.config.max_expanded_chunks
            && self.seed_expanded_count < self.config.max_neighbors_per_seed
    }

    fn remaining_candidate_limit(&self) -> usize {
        let remaining_global = self
            .config
            .max_expanded_chunks
            .saturating_sub(*self.expanded_count);
        let remaining_seed = self
            .config
            .max_neighbors_per_seed
            .saturating_sub(self.seed_expanded_count);
        remaining_global.min(remaining_seed)
    }
}

struct CandidateExpansion<'a> {
    node_id: &'a GraphNodeId,
    score: f32,
    hop_distance: u32,
    graph_path: Vec<GraphExpansionStep>,
}

struct FrontierNode {
    node_id: GraphNodeId,
    path: Vec<GraphExpansionStep>,
}

struct GraphTransition {
    neighbor_node_id: GraphNodeId,
    step: GraphExpansionStep,
}

fn rank_by_chunk_id(hits: &[(ChunkId, f32)]) -> HashMap<ChunkId, usize> {
    hits.iter()
        .enumerate()
        .map(|(rank, (chunk_id, _))| (chunk_id.clone(), rank + 1))
        .collect()
}

fn empty_search_output(include_debug: bool) -> RetrievalSearchOutput {
    RetrievalSearchOutput {
        results: Vec::new(),
        debug: include_debug.then(|| RetrievalDebug {
            dense_vector_path: RetrievalDenseVectorPath::Bm25Only,
            query_embedding_latency_ms: None,
            retrieval_search_sql_statement_count: None,
            local_spans_ms: RetrievalLocalSpansMs::default(),
            candidate_counters: CandidateCounters::default(),
            evidence_pack_mode: RetrievalDebugEvidencePackMode::Full,
            final_evidence_count: 0,
            display_evidence_count: 0,
            bm25_hits: Vec::new(),
            dense_hits: Vec::new(),
            rrf_fused_hits: Vec::new(),
            graph_expanded_hits: Vec::new(),
            reranker: RetrievalRerankDebug::disabled(),
            final_evidence_pack: Vec::new(),
            display_evidence_pack: Vec::new(),
        }),
    }
}

fn graph_expansion_debug_hits(results: &[RetrievalResult]) -> Vec<RetrievalGraphExpansionDebug> {
    results
        .iter()
        .filter_map(|result| {
            let provenance = &result.provenance;
            if provenance.origin != crate::types::RetrievalOrigin::GraphExpansion {
                return None;
            }

            Some(RetrievalGraphExpansionDebug {
                result_rank: provenance.result_rank,
                seed_rank: provenance.seed_rank.unwrap_or(0),
                seed_chunk_id: provenance.seed_chunk_id.clone()?,
                seed_source_id: provenance.seed_source_id.clone()?,
                expanded_chunk_id: result.chunk_id.clone(),
                expanded_source_id: result.chunk.source_id.clone(),
                score: result.score,
                hop_distance: provenance.hop_distance,
                path: provenance.graph_path.clone(),
                reason: "included_by_configured_graph_expansion".into(),
            })
        })
        .collect()
}

fn bounded_rerank_top_n(top_n: usize, candidate_count: usize) -> usize {
    if candidate_count == 0 {
        return 0;
    }
    top_n
        .max(1)
        .min(candidate_count)
        .min(MAX_RERANK_CANDIDATE_CHUNKS)
}

fn rerank_document_text(chunk: &Chunk) -> String {
    let text = chunk
        .context_text
        .as_ref()
        .filter(|context| !context.is_empty())
        .map(|context| format!("{context} {}", chunk.text))
        .unwrap_or_else(|| chunk.text.clone());
    text.chars().take(MAX_RERANK_DOCUMENT_CHARS).collect()
}

fn validated_rerank_hits(
    candidates: &[RerankCandidate],
    hits: Vec<(usize, f32)>,
    top_n: usize,
    submitted_candidate_count: usize,
) -> (Vec<(ChunkId, f32)>, Vec<RetrievalRerankScore>) {
    let mut seen_indices = HashSet::new();
    let submitted_candidate_count = submitted_candidate_count.min(candidates.len());
    let mut valid_hits = hits
        .into_iter()
        .filter(|(index, score)| {
            *index < submitted_candidate_count && score.is_finite() && seen_indices.insert(*index)
        })
        .collect::<Vec<_>>();

    valid_hits.sort_by(|left, right| {
        right
            .1
            .total_cmp(&left.1)
            .then_with(|| left.0.cmp(&right.0))
    });
    valid_hits.truncate(top_n);

    let reranked = valid_hits
        .iter()
        .map(|(index, score)| (candidates[*index].chunk_id.clone(), *score))
        .collect::<Vec<_>>();
    let scores = valid_hits
        .iter()
        .enumerate()
        .map(|(rank, (index, score))| RetrievalRerankScore {
            rank: rank + 1,
            chunk_id: candidates[*index].chunk_id.clone(),
            score: *score,
        })
        .collect();

    (reranked, scores)
}

fn actual_rerank_top_n(top_n: usize, request: Option<&RerankRequestDiagnostics>) -> usize {
    request.map_or(top_n, |request| request.top_n)
}

fn actual_rerank_candidate_count(
    candidate_count: usize,
    request: Option<&RerankRequestDiagnostics>,
) -> usize {
    request.map_or(candidate_count, |request| request.candidate_count)
}

fn with_rerank_diagnostics(
    mut debug: RetrievalRerankDebug,
    diagnostics: Option<&RerankDiagnostics>,
) -> RetrievalRerankDebug {
    let Some(diagnostics) = diagnostics else {
        return debug;
    };
    debug.capability =
        diagnostics
            .capability
            .as_ref()
            .map(|capability| RetrievalRerankCapabilityDebug {
                state: retrieval_rerank_capability_state(capability.state),
                max_context_tokens: capability.max_context_tokens,
                max_candidates: capability.max_candidates,
                max_documents: capability.max_documents,
                max_document_chars: capability.max_document_chars,
                max_payload_chars: capability.max_payload_chars,
                reason: capability.reason.as_deref().map(bounded_rerank_debug_text),
                retried_after_context_limit: diagnostics.retried_after_context_limit,
            });
    debug.request = diagnostics
        .request
        .as_ref()
        .map(|request| RetrievalRerankRequestDebug {
            candidate_count: request.candidate_count,
            document_char_limit: request.document_char_limit,
            top_n: request.top_n,
        });
    debug
}

fn retrieval_rerank_capability_state(
    state: RerankCapabilityState,
) -> RetrievalRerankCapabilityState {
    match state {
        RerankCapabilityState::Cached => RetrievalRerankCapabilityState::Cached,
        RerankCapabilityState::Refreshed => RetrievalRerankCapabilityState::Refreshed,
        RerankCapabilityState::Unavailable => RetrievalRerankCapabilityState::Unavailable,
        RerankCapabilityState::RefreshFailed => RetrievalRerankCapabilityState::RefreshFailed,
    }
}

fn rerank_error_reason_and_diagnostics(
    error: &anyhow::Error,
) -> (String, Option<&RerankDiagnostics>) {
    if let Some(rerank_error) = error.downcast_ref::<RerankError>() {
        return (
            rerank_failure_reason(rerank_error.source_error()),
            Some(rerank_error.diagnostics()),
        );
    }
    (rerank_failure_reason(error), None)
}

fn rerank_failure_reason(error: &anyhow::Error) -> String {
    if let Some(provider_error) = error.downcast_ref::<ProviderError>() {
        return match provider_error {
            ProviderError::Configuration { .. } => "invalid_configuration".to_string(),
            ProviderError::Transport { source, .. } if source.is_timeout() => {
                "request_timeout".to_string()
            }
            ProviderError::Transport { .. } => "request_failed".to_string(),
            ProviderError::HttpStatus { status, .. } => format!("http_status_{}", status.as_u16()),
            ProviderError::ResponseDecode { .. } => "invalid_json".to_string(),
            ProviderError::QueueTimeout { .. } => "queue_timeout".to_string(),
            ProviderError::QueueFull { .. } => "queue_full".to_string(),
            ProviderError::StreamDecode { .. } | ProviderError::MalformedResponse { .. } => {
                "invalid_response".to_string()
            }
        };
    }

    let message = error.to_string().to_ascii_lowercase();
    if message.contains("timeout") || message.contains("timed out") {
        "request_timeout".to_string()
    } else {
        "request_failed".to_string()
    }
}

fn bounded_rerank_debug_text(input: &str) -> String {
    const MAX_DEBUG_TEXT_CHARS: usize = 96;
    input.chars().take(MAX_DEBUG_TEXT_CHARS).collect()
}

/// Rebuild the final evidence-pack debug view after callers alter retrieval result order.
pub fn refresh_final_evidence_pack_debug(debug: &mut RetrievalDebug, results: &[RetrievalResult]) {
    debug.evidence_pack_mode = RetrievalDebugEvidencePackMode::Full;
    refresh_evidence_pack_debug(debug, results);
}

/// Rebuild evidence-pack debug data according to the debug payload mode.
pub fn refresh_evidence_pack_debug(debug: &mut RetrievalDebug, results: &[RetrievalResult]) {
    let selected_canonical_support_ids = selected_display_support_ids(debug);
    match debug.evidence_pack_mode {
        RetrievalDebugEvidencePackMode::Full => {
            debug.final_evidence_pack = final_evidence_pack_debug(results);
            debug.final_evidence_count = debug.final_evidence_pack.len();
            debug.display_evidence_pack = if selected_canonical_support_ids.is_empty() {
                debug.final_evidence_pack.clone()
            } else {
                display_evidence_pack_debug_with_canonical_selection(
                    results,
                    &selected_canonical_support_ids,
                    RetrievalDisplayScope::All,
                )
            };
        }
        RetrievalDebugEvidencePackMode::Compact => {
            debug.final_evidence_pack.clear();
            debug.final_evidence_count = final_evidence_debug_count(results);
            debug.display_evidence_pack = display_evidence_pack_debug_with_canonical_selection(
                results,
                &selected_canonical_support_ids,
                RetrievalDisplayScope::All,
            );
        }
    }
    debug.display_evidence_count = debug.display_evidence_pack.len();
}

fn final_evidence_pack_debug(results: &[RetrievalResult]) -> Vec<RetrievalEvidencePackEntry> {
    evidence_pack_debug_with_display_selection(results, &HashMap::new())
}

fn final_evidence_debug_count(results: &[RetrievalResult]) -> usize {
    let mut seen = HashSet::new();
    let mut count = 0usize;
    for result in results {
        for evidence in &result.evidence_units {
            if seen.insert(evidence.id.0.clone()) {
                count += 1;
            }
        }
    }
    count
}

fn canonical_scope_results(
    results: &[RetrievalResult],
    scope: RetrievalDisplayScope,
) -> Vec<&RetrievalResult> {
    let mut display_results = Vec::new();
    let mut seen = HashSet::new();
    let mut display_index = 0usize;

    for result in results {
        if canonical_multi_evidence_result(result) {
            if let Some(evidence) = result.evidence_units.first() {
                if seen.insert(evidence.id.0.clone()) {
                    if scope.contains(display_index) {
                        display_results.push(result);
                    }
                    display_index += 1;
                }
            }
            continue;
        }

        for evidence in &result.evidence_units {
            if !seen.insert(evidence.id.0.clone()) {
                continue;
            }
            display_index += 1;
        }
    }

    display_results
}

fn selected_display_support_ids(debug: &RetrievalDebug) -> HashMap<String, EvidenceId> {
    if debug.final_evidence_pack.is_empty() && !debug.display_evidence_pack.is_empty() {
        return debug
            .display_evidence_pack
            .iter()
            .map(|entry| (entry.chunk_id.0.clone(), entry.evidence_id.clone()))
            .collect();
    }

    if debug.display_evidence_pack.is_empty()
        || debug.display_evidence_pack.len() >= debug.final_evidence_pack.len()
    {
        return HashMap::new();
    }

    let mut full_counts = HashMap::<String, usize>::new();
    for entry in &debug.final_evidence_pack {
        *full_counts.entry(entry.chunk_id.0.clone()).or_default() += 1;
    }

    let mut display_counts = HashMap::<String, usize>::new();
    for entry in &debug.display_evidence_pack {
        *display_counts.entry(entry.chunk_id.0.clone()).or_default() += 1;
    }

    debug
        .display_evidence_pack
        .iter()
        .filter(|entry| {
            display_counts
                .get(&entry.chunk_id.0)
                .zip(full_counts.get(&entry.chunk_id.0))
                .is_some_and(|(display_count, full_count)| display_count < full_count)
        })
        .map(|entry| (entry.chunk_id.0.clone(), entry.evidence_id.clone()))
        .collect()
}

fn evidence_pack_debug_with_display_selection(
    results: &[RetrievalResult],
    selected_canonical_support_ids: &HashMap<String, EvidenceId>,
) -> Vec<RetrievalEvidencePackEntry> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();

    for result in results {
        for evidence in &result.evidence_units {
            if let Some(selected_id) = selected_canonical_support_ids.get(&result.chunk_id.0) {
                if &evidence.id != selected_id {
                    continue;
                }
            }
            if !seen.insert(evidence.id.0.clone()) {
                continue;
            }

            entries.push(RetrievalEvidencePackEntry {
                label: format!("E{}", entries.len() + 1),
                result_rank: result.provenance.result_rank,
                chunk_id: result.chunk_id.clone(),
                score: result.score,
                evidence_id: evidence.id.clone(),
                source_id: evidence.source_id.clone(),
                role: evidence_debug_role(evidence),
                kind: evidence.kind,
                derived_from: evidence.derived_from.clone(),
                locator: RetrievalLocatorDebug {
                    display: evidence.locator.to_string(),
                    structured: evidence.locator.clone(),
                },
                provenance: result.provenance.clone(),
            });
        }
    }

    entries
}

fn display_evidence_pack_debug_with_canonical_selection(
    results: &[RetrievalResult],
    selected_canonical_support_ids: &HashMap<String, EvidenceId>,
    display_scope: RetrievalDisplayScope,
) -> Vec<RetrievalEvidencePackEntry> {
    let mut entries = Vec::new();
    let mut entry_seen = HashSet::new();
    let mut display_seen = HashSet::new();
    let mut display_index = 0usize;

    for result in results {
        if canonical_multi_evidence_result(result) {
            let Some(first_evidence) = result.evidence_units.first() else {
                continue;
            };
            if !display_seen.insert(first_evidence.id.0.clone()) {
                continue;
            }
            let evidence = if display_scope.contains(display_index) {
                canonical_display_evidence(result, selected_canonical_support_ids)
            } else {
                Some(first_evidence)
            };
            display_index += 1;
            if let Some(evidence) = evidence {
                push_evidence_pack_debug_entry(result, evidence, &mut entries, &mut entry_seen);
            }
            continue;
        }

        for evidence in &result.evidence_units {
            if !display_seen.insert(evidence.id.0.clone()) {
                continue;
            }
            display_index += 1;
            push_evidence_pack_debug_entry(result, evidence, &mut entries, &mut entry_seen);
        }
    }

    entries
}

fn canonical_detail_result<'a>(
    results: &'a [RetrievalResult],
    target: RetrievalCanonicalDetailTarget<'_>,
) -> Option<&'a RetrievalResult> {
    results
        .iter()
        .filter(|result| canonical_multi_evidence_result(result))
        .find(|result| {
            result.evidence_units.iter().any(|evidence| match target {
                RetrievalCanonicalDetailTarget::EvidenceId(evidence_id) => {
                    &evidence.id == evidence_id
                }
                RetrievalCanonicalDetailTarget::Locator(locator) => &evidence.locator == locator,
            })
        })
}

fn canonical_display_evidence<'a>(
    result: &'a RetrievalResult,
    selected_canonical_support_ids: &HashMap<String, EvidenceId>,
) -> Option<&'a EvidenceUnit> {
    selected_canonical_support_ids
        .get(&result.chunk_id.0)
        .and_then(|selected_id| {
            result
                .evidence_units
                .iter()
                .find(|evidence| &evidence.id == selected_id)
        })
        .or_else(|| result.evidence_units.first())
}

fn push_evidence_pack_debug_entry(
    result: &RetrievalResult,
    evidence: &EvidenceUnit,
    entries: &mut Vec<RetrievalEvidencePackEntry>,
    seen: &mut HashSet<String>,
) {
    if !seen.insert(evidence.id.0.clone()) {
        return;
    }

    entries.push(RetrievalEvidencePackEntry {
        label: format!("E{}", entries.len() + 1),
        result_rank: result.provenance.result_rank,
        chunk_id: result.chunk_id.clone(),
        score: result.score,
        evidence_id: evidence.id.clone(),
        source_id: evidence.source_id.clone(),
        role: evidence_debug_role(evidence),
        kind: evidence.kind,
        derived_from: evidence.derived_from.clone(),
        locator: RetrievalLocatorDebug {
            display: evidence.locator.to_string(),
            structured: evidence.locator.clone(),
        },
        provenance: result.provenance.clone(),
    });
}

fn canonical_display_support_ids(
    query: &str,
    results: &[&RetrievalResult],
    semantic_scores: &HashMap<String, f32>,
) -> HashMap<String, EvidenceId> {
    results
        .iter()
        .copied()
        .filter(|result| canonical_multi_evidence_result(result))
        .filter_map(|result| {
            let evidence = best_support_evidence(query, &result.evidence_units, semantic_scores)?;
            Some((result.chunk_id.0.clone(), evidence.id.clone()))
        })
        .collect()
}

fn best_support_evidence<'a>(
    query: &str,
    evidence_units: &'a [EvidenceUnit],
    semantic_scores: &HashMap<String, f32>,
) -> Option<&'a EvidenceUnit> {
    evidence_units
        .iter()
        .enumerate()
        .max_by(|(left_index, left), (right_index, right)| {
            compare_support_candidates(
                query,
                left,
                *left_index,
                right,
                *right_index,
                semantic_scores,
            )
        })
        .map(|(_, evidence)| evidence)
}

fn compare_support_candidates(
    query: &str,
    left: &EvidenceUnit,
    left_index: usize,
    right: &EvidenceUnit,
    right_index: usize,
    semantic_scores: &HashMap<String, f32>,
) -> std::cmp::Ordering {
    let left_semantic = semantic_scores.get(&left.id.0).copied();
    let right_semantic = semantic_scores.get(&right.id.0).copied();
    support_score_tuple(query, left, left_semantic, left_index).cmp(&support_score_tuple(
        query,
        right,
        right_semantic,
        right_index,
    ))
}

fn support_score_tuple(
    query: &str,
    evidence: &EvidenceUnit,
    semantic_score: Option<f32>,
    index: usize,
) -> (i32, i32, i32, std::cmp::Reverse<usize>) {
    let finite_semantic = semantic_score.filter(|score| score.is_finite());
    let semantic = finite_semantic
        .map(|score| (score * 1_000_000.0).round() as i32)
        .unwrap_or(0);
    let has_semantic = i32::from(finite_semantic.is_some());
    let lexical = (lexical_support_score(query, &evidence.text) * 1_000.0).round() as i32;
    (has_semantic, semantic, lexical, std::cmp::Reverse(index))
}

fn canonical_multi_evidence_result(result: &RetrievalResult) -> bool {
    result.evidence_units.len() > 1
        && result
            .evidence_units
            .iter()
            .all(|evidence| matches!(evidence.locator, SourceLocator::Canonical { .. }))
}

fn lexical_support_score(query: &str, text: &str) -> f32 {
    let query_terms = support_terms(query);
    if query_terms.is_empty() {
        return 0.0;
    }

    let text_terms = support_terms(text);
    if text_terms.is_empty() {
        return 0.0;
    }

    let text_term_set = text_terms.into_iter().collect::<HashSet<_>>();
    let hits = query_terms
        .iter()
        .filter(|term| text_term_set.contains(*term))
        .count();
    hits as f32 / query_terms.len() as f32
}

fn support_terms(input: &str) -> Vec<String> {
    input
        .split(|ch: char| !ch.is_alphanumeric())
        .map(str::trim)
        .filter(|term| !term.is_empty())
        .map(str::to_lowercase)
        .collect()
}

fn cosine_similarity(left: &[f32], right: &[f32]) -> Option<f32> {
    if left.len() != right.len() || left.is_empty() {
        return None;
    }

    let mut dot = 0.0f32;
    let mut left_norm = 0.0f32;
    let mut right_norm = 0.0f32;
    for (left, right) in left.iter().zip(right) {
        dot += left * right;
        left_norm += left * left;
        right_norm += right * right;
    }

    if left_norm == 0.0 || right_norm == 0.0 {
        return None;
    }

    Some(dot / (left_norm.sqrt() * right_norm.sqrt()))
}

fn evidence_debug_role(evidence: &EvidenceUnit) -> RetrievalEvidenceRole {
    match evidence.kind {
        EvidenceKind::Text => RetrievalEvidenceRole::OriginalText,
        EvidenceKind::Ocr => RetrievalEvidenceRole::OcrText,
        EvidenceKind::Image => RetrievalEvidenceRole::ImageArtifact,
        EvidenceKind::Generated if evidence.derived_from.is_some() => {
            RetrievalEvidenceRole::ImageCaptionGenerated
        }
        EvidenceKind::Generated => RetrievalEvidenceRole::Generated,
    }
}

fn rrf_fusion(dense: &[(ChunkId, f32)], bm25: &[(ChunkId, f32)], k: usize) -> Vec<(ChunkId, f32)> {
    let mut scores: HashMap<ChunkId, f32> = HashMap::new();

    for (rank, (id, _)) in dense.iter().enumerate() {
        *scores.entry(id.clone()).or_default() += 1.0 / (k as f32 + rank as f32 + 1.0);
    }

    for (rank, (id, _)) in bm25.iter().enumerate() {
        *scores.entry(id.clone()).or_default() += 1.0 / (k as f32 + rank as f32 + 1.0);
    }

    let mut results: Vec<(ChunkId, f32)> = scores.into_iter().collect();
    results.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
    results
}

fn derived_graph_score(seed_score: f32, hop: usize) -> f32 {
    let distance = hop.max(1) as i32;
    seed_score * GRAPH_EXPANSION_SCORE_DECAY.powi(distance)
}

fn graph_step(
    edge: &crate::types::GraphEdge,
    direction: GraphTraversalDirection,
) -> GraphExpansionStep {
    GraphExpansionStep {
        edge_type: edge.edge_type,
        from_node_id: edge.from_node_id.clone(),
        to_node_id: edge.to_node_id.clone(),
        direction,
    }
}

fn sort_edges(edges: &mut [crate::types::GraphEdge]) {
    edges.sort_by(|left, right| {
        left.edge_type
            .as_str()
            .cmp(right.edge_type.as_str())
            .then_with(|| left.ordinal.cmp(&right.ordinal))
            .then_with(|| left.id.0.cmp(&right.id.0))
    });
}

fn direction_key(direction: &GraphTraversalDirection) -> &'static str {
    match direction {
        GraphTraversalDirection::Outgoing => "outgoing",
        GraphTraversalDirection::Incoming => "incoming",
    }
}

fn locator_page(locator: &SourceLocator) -> Option<u32> {
    match locator {
        SourceLocator::Pdf { page, .. }
        | SourceLocator::PdfOcr { page, .. }
        | SourceLocator::PdfImage { page, .. } => Some(*page),
        SourceLocator::Document { .. }
        | SourceLocator::Markdown { .. }
        | SourceLocator::Canonical { .. } => None,
    }
}

fn push_unique_node_id(
    node_ids: &mut Vec<GraphNodeId>,
    seen: &mut HashSet<String>,
    node_id: GraphNodeId,
) {
    if seen.insert(node_id.0.clone()) {
        node_ids.push(node_id);
    }
}

fn push_unique_evidence(
    evidence_units: &mut Vec<EvidenceUnit>,
    seen: &mut HashSet<String>,
    unit: EvidenceUnit,
) {
    if seen.insert(unit.id.0.clone()) {
        evidence_units.push(unit);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::RerankStrategy;
    use crate::traits::RerankResponse;
    use async_trait::async_trait;
    #[cfg(feature = "qdrant")]
    use std::io::{Read, Write};
    #[cfg(feature = "qdrant")]
    use std::net::{TcpListener, TcpStream};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::Mutex;
    #[cfg(feature = "qdrant")]
    use std::thread;
    use std::time::Duration;

    use crate::index::hnsw::HnswIndex;
    use crate::index::sqlite_fts::SqliteFtsIndex;
    #[cfg(feature = "qdrant")]
    use crate::store::EmbeddingProfileConfig;
    use crate::store::Store;
    use crate::traits::{LexicalIndex, VectorDocument, VectorIndex};
    use crate::types::{
        BBox, CanonicalLocator, EdgeType, EvidenceId, EvidenceKind, GraphEdge, GraphEdgeId,
        GraphNode, GraphNodeId, GraphNodeKind, GraphTraversalDirection, ImageArtifact, ImageId,
        ReferenceComponent, RetrievalDenseVectorPath, RetrievalEvidenceRole, RetrievalOrigin,
        RetrievalRerankStatus, Source, SourceLocator, SourceStatus, VectorIndexResidency,
    };

    #[test]
    fn rrf_merges_rankings() {
        let dense = vec![
            (ChunkId("a".into()), 0.9),
            (ChunkId("b".into()), 0.8),
            (ChunkId("c".into()), 0.7),
        ];
        let bm25 = vec![
            (ChunkId("b".into()), 5.0),
            (ChunkId("d".into()), 4.0),
            (ChunkId("a".into()), 3.0),
        ];

        let fused = rrf_fusion(&dense, &bm25, 60);

        assert!(!fused.is_empty());
        let ids: Vec<&str> = fused.iter().map(|(id, _)| id.0.as_str()).collect();
        assert!(ids.contains(&"a"));
        assert!(ids.contains(&"b"));
        assert!(ids.contains(&"c"));
        assert!(ids.contains(&"d"));

        let b_score = fused.iter().find(|(id, _)| id.0 == "b").unwrap().1;
        let c_score = fused.iter().find(|(id, _)| id.0 == "c").unwrap().1;
        assert!(b_score > c_score);
    }

    #[test]
    fn rrf_deduplicates() {
        let dense = vec![(ChunkId("x".into()), 1.0), (ChunkId("x".into()), 0.9)];
        let bm25 = vec![(ChunkId("x".into()), 5.0)];

        let fused = rrf_fusion(&dense, &bm25, 60);
        let x_entries: Vec<_> = fused.iter().filter(|(id, _)| id.0 == "x").collect();
        assert_eq!(x_entries.len(), 1);
    }

    #[test]
    fn rrf_empty_inputs() {
        let fused = rrf_fusion(&[], &[], 60);
        assert!(fused.is_empty());
    }

    struct KeywordEmbeddingClient;

    #[async_trait]
    impl EmbeddingClient for KeywordEmbeddingClient {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            Ok(texts.iter().map(|text| keyword_vector(text)).collect())
        }

        fn dimension(&self) -> usize {
            2
        }
    }

    struct RecordingEmbeddingClient {
        calls: AtomicUsize,
    }

    impl RecordingEmbeddingClient {
        fn new() -> Self {
            Self {
                calls: AtomicUsize::new(0),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }
    }

    #[async_trait]
    impl EmbeddingClient for RecordingEmbeddingClient {
        async fn embed(&self, _texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.calls.fetch_add(1, Ordering::SeqCst);
            Err(anyhow!("embedding should not be called in BM25-only mode"))
        }

        fn dimension(&self) -> usize {
            2
        }
    }

    struct RecordingQueryEmbeddingClient {
        texts: Mutex<Vec<Vec<String>>>,
    }

    impl RecordingQueryEmbeddingClient {
        fn new() -> Self {
            Self {
                texts: Mutex::new(Vec::new()),
            }
        }

        fn recorded_texts(&self) -> Vec<Vec<String>> {
            self.texts.lock().unwrap().clone()
        }
    }

    #[async_trait]
    impl EmbeddingClient for RecordingQueryEmbeddingClient {
        async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
            self.texts.lock().unwrap().push(texts.to_vec());
            Ok(texts.iter().map(|text| keyword_vector(text)).collect())
        }

        fn dimension(&self) -> usize {
            2
        }
    }

    fn keyword_vector(text: &str) -> Vec<f32> {
        let lower = text.to_ascii_lowercase();
        if lower.contains("alpha") {
            vec![1.0, 0.0]
        } else if lower.contains("beta") {
            vec![0.0, 1.0]
        } else {
            vec![0.5, 0.5]
        }
    }

    #[cfg(feature = "qdrant")]
    fn qdrant_config(url: String) -> QdrantConfig {
        QdrantConfig {
            enabled: true,
            url,
            collection: "verbatim".into(),
            prefer_for_search: true,
            timeout_seconds: 2,
        }
    }

    #[cfg(feature = "qdrant")]
    fn spawn_qdrant_search_response(
        status: u16,
        body: &'static str,
    ) -> (String, thread::JoinHandle<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind qdrant search server");
        let addr = listener.local_addr().expect("qdrant search server addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept qdrant search");
            let request_line = read_request_line(&mut stream);
            write_http_response(&mut stream, status, body);
            request_line
        });
        (format!("http://{addr}"), handle)
    }

    #[cfg(feature = "qdrant")]
    fn spawn_qdrant_search_response_with_body(
        status: u16,
        body: &'static str,
    ) -> (String, thread::JoinHandle<(String, String)>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind qdrant search server");
        let addr = listener.local_addr().expect("qdrant search server addr");
        let handle = thread::spawn(move || {
            let (mut stream, _) = listener.accept().expect("accept qdrant search");
            let request = read_qdrant_request(&mut stream);
            write_http_response(&mut stream, status, body);
            request
        });
        (format!("http://{addr}"), handle)
    }

    #[cfg(feature = "qdrant")]
    fn spawn_optional_qdrant_search_response(
        status: u16,
        body: &'static str,
    ) -> (String, thread::JoinHandle<Option<String>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind qdrant search server");
        listener
            .set_nonblocking(true)
            .expect("set qdrant listener nonblocking");
        let addr = listener.local_addr().expect("qdrant search server addr");
        let handle = thread::spawn(move || {
            let deadline = std::time::Instant::now() + std::time::Duration::from_millis(300);
            loop {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let request_line = read_request_line(&mut stream);
                        write_http_response(&mut stream, status, body);
                        return Some(request_line);
                    }
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        if std::time::Instant::now() >= deadline {
                            return None;
                        }
                        thread::sleep(std::time::Duration::from_millis(10));
                    }
                    Err(error) => panic!("accept qdrant search: {error}"),
                }
            }
        });
        (format!("http://{addr}"), handle)
    }

    #[cfg(feature = "qdrant")]
    fn read_request_line(stream: &mut TcpStream) -> String {
        read_qdrant_request(stream).0
    }

    #[cfg(feature = "qdrant")]
    fn read_qdrant_request(stream: &mut TcpStream) -> (String, String) {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk).expect("read qdrant request");
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..n]);
            let text = String::from_utf8_lossy(&buffer);
            let Some((head, body)) = text.split_once("\r\n\r\n") else {
                continue;
            };
            let content_len = head
                .lines()
                .find_map(|line| {
                    let (name, value) = line.split_once(':')?;
                    name.eq_ignore_ascii_case("content-length")
                        .then(|| value.trim().parse::<usize>().ok())
                        .flatten()
                })
                .unwrap_or(0);
            if body.len() >= content_len {
                break;
            }
        }
        let text = String::from_utf8(buffer).expect("request utf8");
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        (
            head.lines().next().unwrap_or_default().to_string(),
            body.to_string(),
        )
    }

    #[cfg(feature = "qdrant")]
    fn write_http_response(stream: &mut TcpStream, status: u16, body: &str) {
        let reason = if status == 200 { "OK" } else { "ERR" };
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .expect("write qdrant response");
    }

    struct StaticVectorIndex {
        hits: Vec<(ChunkId, f32)>,
    }

    impl StaticVectorIndex {
        fn new(hits: Vec<(ChunkId, f32)>) -> Self {
            Self { hits }
        }
    }

    impl VectorIndex for StaticVectorIndex {
        fn upsert(&mut self, _document: VectorDocument) {}

        fn delete_source(&mut self, _source_id: &SourceId) -> Result<()> {
            Ok(())
        }

        fn search(&self, _query: &[f32], top_k: usize) -> Vec<(ChunkId, f32)> {
            self.hits.iter().take(top_k).cloned().collect()
        }

        fn rebuild_from_store(&mut self, _store: &Store) -> Result<()> {
            Ok(())
        }

        fn len(&self) -> usize {
            self.hits.len()
        }
    }

    struct StaticLexicalIndex {
        hits: Vec<(ChunkId, f32)>,
    }

    impl StaticLexicalIndex {
        fn new(hits: Vec<(ChunkId, f32)>) -> Self {
            Self { hits }
        }
    }

    impl LexicalIndex for StaticLexicalIndex {
        fn upsert(&self, _document: &crate::traits::LexicalDocument) -> Result<()> {
            Ok(())
        }

        fn delete_source(&self, _source_id: &SourceId) -> Result<()> {
            Ok(())
        }

        fn search(&self, _query: &str, top_k: usize) -> Result<Vec<(ChunkId, f32)>> {
            Ok(self.hits.iter().take(top_k).cloned().collect())
        }

        fn rebuild_from_store(&self, _store: &Store) -> Result<()> {
            Ok(())
        }
    }

    enum MockRerankResponse {
        Hits(Vec<(usize, f32)>),
        HitsWithRequest {
            hits: Vec<(usize, f32)>,
            request: RerankRequestDiagnostics,
        },
        Error(&'static str),
    }

    struct RecordingReranker {
        response: MockRerankResponse,
        calls: AtomicUsize,
        docs: Mutex<Vec<Vec<String>>>,
        top_ns: Mutex<Vec<usize>>,
    }

    impl RecordingReranker {
        fn hits(hits: Vec<(usize, f32)>) -> Self {
            Self {
                response: MockRerankResponse::Hits(hits),
                calls: AtomicUsize::new(0),
                docs: Mutex::new(Vec::new()),
                top_ns: Mutex::new(Vec::new()),
            }
        }

        fn hits_with_request(hits: Vec<(usize, f32)>, request: RerankRequestDiagnostics) -> Self {
            Self {
                response: MockRerankResponse::HitsWithRequest { hits, request },
                calls: AtomicUsize::new(0),
                docs: Mutex::new(Vec::new()),
                top_ns: Mutex::new(Vec::new()),
            }
        }

        fn error(message: &'static str) -> Self {
            Self {
                response: MockRerankResponse::Error(message),
                calls: AtomicUsize::new(0),
                docs: Mutex::new(Vec::new()),
                top_ns: Mutex::new(Vec::new()),
            }
        }

        fn call_count(&self) -> usize {
            self.calls.load(Ordering::SeqCst)
        }

        fn recorded_docs(&self) -> Vec<Vec<String>> {
            self.docs.lock().unwrap().clone()
        }

        fn recorded_top_ns(&self) -> Vec<usize> {
            self.top_ns.lock().unwrap().clone()
        }

        fn record_call(&self, docs: &[String], top_n: usize) {
            self.calls.fetch_add(1, Ordering::SeqCst);
            self.docs.lock().unwrap().push(docs.to_vec());
            self.top_ns.lock().unwrap().push(top_n);
        }
    }

    #[async_trait]
    impl Reranker for RecordingReranker {
        async fn rerank(
            &self,
            _query: &str,
            docs: &[String],
            top_n: usize,
        ) -> Result<Vec<(usize, f32)>> {
            self.record_call(docs, top_n);
            match &self.response {
                MockRerankResponse::Hits(hits) => Ok(hits.clone()),
                MockRerankResponse::HitsWithRequest { hits, .. } => Ok(hits.clone()),
                MockRerankResponse::Error(message) => Err(anyhow!(*message)),
            }
        }

        async fn rerank_with_diagnostics(
            &self,
            _query: &str,
            docs: &[String],
            top_n: usize,
        ) -> Result<RerankResponse> {
            self.record_call(docs, top_n);
            match &self.response {
                MockRerankResponse::Hits(hits) => Ok(RerankResponse {
                    hits: hits.clone(),
                    diagnostics: RerankDiagnostics::default(),
                }),
                MockRerankResponse::HitsWithRequest { hits, request } => Ok(RerankResponse {
                    hits: hits.clone(),
                    diagnostics: RerankDiagnostics {
                        request: Some(request.clone()),
                        ..RerankDiagnostics::default()
                    },
                }),
                MockRerankResponse::Error(message) => Err(anyhow!(*message)),
            }
        }
    }

    fn source(id: &str) -> Source {
        Source {
            id: SourceId(id.into()),
            path: std::path::PathBuf::from(format!("/tmp/{id}.txt")),
            hash: format!("hash-{id}"),
            status: SourceStatus::Indexed,
            parser_used: Some("plaintext".into()),
            last_ingested_at: None,
        }
    }

    fn insert_child(store: &Store, source: &Source, chunk_id: &str, text: &str) -> Chunk {
        let evidence = EvidenceUnit {
            id: crate::types::EvidenceId(format!("ev-{chunk_id}")),
            source_id: source.id.clone(),
            kind: EvidenceKind::Text,
            derived_from: None,
            locator: SourceLocator::Document {
                path_or_url: source.path.to_string_lossy().into_owned(),
                line_start: 1,
                line_end: None,
            },
            text: text.into(),
            text_hash: format!("hash-{chunk_id}"),
            heading_path: Vec::new(),
            position: 0,
        };
        let chunk = Chunk {
            id: ChunkId(chunk_id.into()),
            source_id: source.id.clone(),
            chunk_hash: format!("hash-{chunk_id}"),
            embedding_input_hash: None,
            text: text.into(),
            context_text: None,
            token_count: 4,
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: Vec::new(),
            evidence_unit_ids: vec![evidence.id.clone()],
        };

        store.add_source(source).unwrap();
        store.bulk_insert_evidence(&[evidence]).unwrap();
        store
            .bulk_insert_chunks(std::slice::from_ref(&chunk))
            .unwrap();
        store
            .link_chunk_evidence(&[(chunk.id.clone(), chunk.evidence_unit_ids[0].clone())])
            .unwrap();
        chunk
    }

    fn insert_text_chunk(store: &Store, source: &Source, chunk_id: &str, text: &str) -> Chunk {
        TextChunkFixture::new(store, source, chunk_id, text).insert()
    }

    fn insert_text_chunk_with_context(
        store: &Store,
        source: &Source,
        chunk_id: &str,
        text: &str,
        context_text: &str,
    ) -> Chunk {
        TextChunkFixture::new(store, source, chunk_id, text)
            .with_context_text(context_text)
            .insert()
    }

    fn insert_pdf_text_chunk(
        store: &Store,
        source: &Source,
        chunk_id: &str,
        text: &str,
        page: u32,
    ) -> Chunk {
        TextChunkFixture::new(store, source, chunk_id, text)
            .with_locator(SourceLocator::Pdf {
                page,
                paragraph: 1,
                bbox: None,
            })
            .insert()
    }

    fn insert_text_chunk_with_heading(
        store: &Store,
        source: &Source,
        chunk_id: &str,
        text: &str,
        heading_path: Vec<String>,
    ) -> Chunk {
        TextChunkFixture::new(store, source, chunk_id, text)
            .with_heading_path(heading_path)
            .insert()
    }

    fn insert_text_chunk_with_parent(
        store: &Store,
        source: &Source,
        chunk_id: &str,
        text: &str,
        parent_chunk_id: ChunkId,
    ) -> Chunk {
        TextChunkFixture::new(store, source, chunk_id, text)
            .with_parent_chunk_id(parent_chunk_id)
            .insert()
    }

    fn insert_canonical_chunk(
        store: &Store,
        source: &Source,
        chunk_id: &str,
        evidence: &[(&str, u32, &str)],
    ) -> Chunk {
        let evidence_units = evidence
            .iter()
            .map(|(id, verse, text)| EvidenceUnit {
                id: EvidenceId((*id).into()),
                source_id: source.id.clone(),
                kind: EvidenceKind::Text,
                derived_from: None,
                locator: SourceLocator::Canonical {
                    locator: CanonicalLocator::single_unit(
                        "bible",
                        "CSB",
                        vec![
                            ReferenceComponent {
                                level: "book".into(),
                                value: "2 Timothy".into(),
                                ordinal: Some(55),
                            },
                            ReferenceComponent {
                                level: "chapter".into(),
                                value: "4".into(),
                                ordinal: Some(4),
                            },
                            ReferenceComponent {
                                level: "verse".into(),
                                value: verse.to_string(),
                                ordinal: Some(*verse),
                            },
                        ],
                        format!("2 Timothy 4:{verse}"),
                        format!("2timothy:4:{verse}"),
                    ),
                },
                text: (*text).into(),
                text_hash: format!("hash-{id}"),
                heading_path: Vec::new(),
                position: *verse,
            })
            .collect::<Vec<_>>();
        let chunk = Chunk {
            id: ChunkId(chunk_id.into()),
            source_id: source.id.clone(),
            chunk_hash: format!("hash-{chunk_id}"),
            embedding_input_hash: None,
            text: evidence_units
                .iter()
                .map(|unit| unit.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            context_text: None,
            token_count: evidence_units.len() as u32 * 4,
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: Vec::new(),
            evidence_unit_ids: evidence_units.iter().map(|unit| unit.id.clone()).collect(),
        };

        store.bulk_insert_evidence(&evidence_units).unwrap();
        store
            .bulk_insert_chunks(std::slice::from_ref(&chunk))
            .unwrap();
        let links = chunk
            .evidence_unit_ids
            .iter()
            .cloned()
            .map(|evidence_id| (chunk.id.clone(), evidence_id))
            .collect::<Vec<_>>();
        store.link_chunk_evidence(&links).unwrap();
        chunk
    }

    fn insert_numbered_canonical_chunk(
        store: &Store,
        source: &Source,
        chunk_id: &str,
        evidence_id_prefix: &str,
        start_verse: u32,
        evidence_count: usize,
        support_offset: usize,
        marker: &str,
    ) -> Chunk {
        assert!(support_offset < evidence_count);
        let evidence_units = (0..evidence_count)
            .map(|index| {
                let verse = start_verse + u32::try_from(index).expect("evidence index fits in u32");
                let id = numbered_canonical_evidence_id(evidence_id_prefix, start_verse, index);
                let text = if index == support_offset {
                    format!("{marker} alpha crown support verse {verse}.")
                } else {
                    format!("{marker} background verse {verse}.")
                };

                EvidenceUnit {
                    id: EvidenceId(id.clone()),
                    source_id: source.id.clone(),
                    kind: EvidenceKind::Text,
                    derived_from: None,
                    locator: SourceLocator::Canonical {
                        locator: CanonicalLocator::single_unit(
                            "bible",
                            "CSB",
                            vec![
                                ReferenceComponent {
                                    level: "book".into(),
                                    value: "2 Timothy".into(),
                                    ordinal: Some(55),
                                },
                                ReferenceComponent {
                                    level: "chapter".into(),
                                    value: "4".into(),
                                    ordinal: Some(4),
                                },
                                ReferenceComponent {
                                    level: "verse".into(),
                                    value: verse.to_string(),
                                    ordinal: Some(verse),
                                },
                            ],
                            format!("2 Timothy 4:{verse}"),
                            format!("2timothy:4:{verse}"),
                        ),
                    },
                    text,
                    text_hash: format!("hash-{id}"),
                    heading_path: Vec::new(),
                    position: verse,
                }
            })
            .collect::<Vec<_>>();
        let chunk = Chunk {
            id: ChunkId(chunk_id.into()),
            source_id: source.id.clone(),
            chunk_hash: format!("hash-{chunk_id}"),
            embedding_input_hash: None,
            text: evidence_units
                .iter()
                .map(|unit| unit.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            context_text: None,
            token_count: evidence_units.len() as u32 * 4,
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: Vec::new(),
            evidence_unit_ids: evidence_units.iter().map(|unit| unit.id.clone()).collect(),
        };

        store.bulk_insert_evidence(&evidence_units).unwrap();
        store
            .bulk_insert_chunks(std::slice::from_ref(&chunk))
            .unwrap();
        let links = chunk
            .evidence_unit_ids
            .iter()
            .cloned()
            .map(|evidence_id| (chunk.id.clone(), evidence_id))
            .collect::<Vec<_>>();
        store.link_chunk_evidence(&links).unwrap();
        chunk
    }

    fn numbered_canonical_evidence_id(prefix: &str, start_verse: u32, index: usize) -> String {
        let verse = start_verse + u32::try_from(index).expect("evidence index fits in u32");
        format!("{prefix}-{verse:03}")
    }

    struct TextChunkFixture<'a> {
        store: &'a Store,
        source: &'a Source,
        chunk_id: &'a str,
        text: &'a str,
        locator: SourceLocator,
        heading_path: Vec<String>,
        parent_chunk_id: Option<ChunkId>,
        context_text: Option<String>,
    }

    impl<'a> TextChunkFixture<'a> {
        fn new(store: &'a Store, source: &'a Source, chunk_id: &'a str, text: &'a str) -> Self {
            Self {
                store,
                source,
                chunk_id,
                text,
                locator: SourceLocator::Document {
                    path_or_url: source.path.to_string_lossy().into_owned(),
                    line_start: 1,
                    line_end: None,
                },
                heading_path: Vec::new(),
                parent_chunk_id: None,
                context_text: None,
            }
        }

        fn with_locator(mut self, locator: SourceLocator) -> Self {
            self.locator = locator;
            self
        }

        fn with_heading_path(mut self, heading_path: Vec<String>) -> Self {
            self.heading_path = heading_path;
            self
        }

        fn with_parent_chunk_id(mut self, parent_chunk_id: ChunkId) -> Self {
            self.parent_chunk_id = Some(parent_chunk_id);
            self
        }

        fn with_context_text(mut self, context_text: &str) -> Self {
            self.context_text = Some(context_text.to_string());
            self
        }

        fn insert(self) -> Chunk {
            let evidence = EvidenceUnit {
                id: EvidenceId(format!("ev-{}", self.chunk_id)),
                source_id: self.source.id.clone(),
                kind: EvidenceKind::Text,
                derived_from: None,
                locator: self.locator,
                text: self.text.into(),
                text_hash: format!("hash-{}", self.chunk_id),
                heading_path: self.heading_path.clone(),
                position: 0,
            };
            let chunk = Chunk {
                id: ChunkId(self.chunk_id.into()),
                source_id: self.source.id.clone(),
                chunk_hash: format!("hash-{}", self.chunk_id),
                embedding_input_hash: None,
                text: self.text.into(),
                context_text: self.context_text,
                token_count: 4,
                chunk_type: ChunkType::Child,
                parent_chunk_id: self.parent_chunk_id,
                heading_path: self.heading_path,
                evidence_unit_ids: vec![evidence.id.clone()],
            };
            self.store.bulk_insert_evidence(&[evidence]).unwrap();
            self.store
                .bulk_insert_chunks(std::slice::from_ref(&chunk))
                .unwrap();
            self.store
                .link_chunk_evidence(&[(chunk.id.clone(), chunk.evidence_unit_ids[0].clone())])
                .unwrap();
            chunk
        }
    }

    fn insert_parent_chunk(
        store: &Store,
        source: &Source,
        chunk_id: &str,
        text: &str,
        evidence_ids: Vec<EvidenceId>,
    ) -> Chunk {
        let chunk = Chunk {
            id: ChunkId(chunk_id.into()),
            source_id: source.id.clone(),
            chunk_hash: format!("hash-{chunk_id}"),
            embedding_input_hash: None,
            text: text.into(),
            context_text: None,
            token_count: 12,
            chunk_type: ChunkType::Parent,
            parent_chunk_id: None,
            heading_path: Vec::new(),
            evidence_unit_ids: evidence_ids,
        };
        store
            .bulk_insert_chunks(std::slice::from_ref(&chunk))
            .unwrap();
        let links = chunk
            .evidence_unit_ids
            .iter()
            .cloned()
            .map(|evidence_id| (chunk.id.clone(), evidence_id))
            .collect::<Vec<_>>();
        store.link_chunk_evidence(&links).unwrap();
        chunk
    }

    fn insert_image_chunk(
        store: &Store,
        source: &Source,
        chunk_id: &str,
        image_id: &str,
        page: u32,
    ) -> (Chunk, ImageArtifact) {
        let evidence_id = EvidenceId(format!("ev-{chunk_id}"));
        let evidence = EvidenceUnit {
            id: evidence_id.clone(),
            source_id: source.id.clone(),
            kind: EvidenceKind::Image,
            derived_from: None,
            locator: SourceLocator::PdfImage {
                page,
                image_index: 1,
                bbox: Some(BBox {
                    x0: 1.0,
                    y0: 2.0,
                    x1: 3.0,
                    y1: 4.0,
                }),
            },
            text: "Image evidence artifact.".into(),
            text_hash: format!("hash-{chunk_id}"),
            heading_path: Vec::new(),
            position: 1,
        };
        let chunk = Chunk {
            id: ChunkId(chunk_id.into()),
            source_id: source.id.clone(),
            chunk_hash: format!("hash-{chunk_id}"),
            embedding_input_hash: None,
            text: "Image evidence artifact.".into(),
            context_text: None,
            token_count: 4,
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: Vec::new(),
            evidence_unit_ids: vec![evidence_id.clone()],
        };
        let artifact = ImageArtifact {
            image_id: ImageId(image_id.into()),
            source_id: source.id.clone(),
            evidence_id,
            relative_path: std::path::PathBuf::from(format!(
                "image-artifacts/{}/{}.png",
                source.id.0, image_id
            )),
            content_hash: format!("hash-{image_id}"),
            mime_type: "image/png".into(),
            width: 100,
            height: 80,
            page,
            image_index: 1,
            bbox: None,
        };

        store.bulk_insert_evidence(&[evidence]).unwrap();
        store
            .bulk_insert_chunks(std::slice::from_ref(&chunk))
            .unwrap();
        store
            .link_chunk_evidence(&[(chunk.id.clone(), chunk.evidence_unit_ids[0].clone())])
            .unwrap();
        store
            .bulk_insert_image_artifacts(std::slice::from_ref(&artifact))
            .unwrap();

        (chunk, artifact)
    }

    fn graph_node(source_id: &SourceId, kind: GraphNodeKind, external_id: &str) -> GraphNode {
        GraphNode {
            id: GraphNodeId::new(source_id, kind, external_id),
            source_id: source_id.clone(),
            kind,
            external_id: external_id.into(),
            label: None,
            locator: None,
            ordinal: None,
            metadata: None,
        }
    }

    fn graph_edge(
        source_id: &SourceId,
        edge_type: EdgeType,
        from_node_id: &GraphNodeId,
        to_node_id: &GraphNodeId,
        ordinal: u32,
    ) -> GraphEdge {
        GraphEdge {
            id: GraphEdgeId::new(
                source_id,
                edge_type,
                from_node_id,
                to_node_id,
                Some(ordinal),
            ),
            source_id: source_id.clone(),
            edge_type,
            from_node_id: from_node_id.clone(),
            to_node_id: to_node_id.clone(),
            ordinal: Some(ordinal),
            weight: None,
            metadata: None,
        }
    }

    fn upsert_chunk_graph_nodes(store: &Store, source_id: &SourceId, chunks: &[&Chunk]) {
        let nodes = chunks
            .iter()
            .map(|chunk| graph_node(source_id, GraphNodeKind::Chunk, &chunk.id.0))
            .collect::<Vec<_>>();
        store.upsert_graph_nodes(&nodes).unwrap();
    }

    fn hnsw_with_seed(store: &Store, seed: &Chunk) -> HnswIndex {
        store
            .replace_all_vector_documents(&[VectorDocument {
                chunk_id: seed.id.clone(),
                source_id: seed.source_id.clone(),
                vector: keyword_vector(&seed.text),
            }])
            .unwrap();
        let mut hnsw = HnswIndex::new();
        hnsw.rebuild_from_store(store).unwrap();
        hnsw
    }

    fn graph_config(edge_types: Vec<EdgeType>) -> GraphConfig {
        GraphConfig {
            enabled: true,
            max_hops: 1,
            max_expanded_chunks: 30,
            max_neighbors_per_seed: 6,
            edge_types,
            extraction: Default::default(),
            global_search: Default::default(),
        }
    }

    include!("retrieve/candidate_telemetry_tests.rs");

    #[tokio::test]
    async fn retrieval_source_set_filter_includes_union_and_excludes_non_members() {
        let store = Store::in_memory().unwrap();
        let first = source("src-articles");
        let second = source("src-areskapitalon");
        let outside = source("src-outside");
        let alpha = insert_child(&store, &first, "chunk-alpha", "alpha content");
        let beta = insert_child(&store, &second, "chunk-beta", "beta content");
        let outside_beta = insert_child(&store, &outside, "chunk-outside", "beta outside");
        store
            .replace_all_vector_documents(&[
                VectorDocument {
                    chunk_id: alpha.id.clone(),
                    source_id: first.id.clone(),
                    vector: keyword_vector(&alpha.text),
                },
                VectorDocument {
                    chunk_id: beta.id.clone(),
                    source_id: second.id.clone(),
                    vector: keyword_vector(&beta.text),
                },
                VectorDocument {
                    chunk_id: outside_beta.id.clone(),
                    source_id: outside.id.clone(),
                    vector: keyword_vector(&outside_beta.text),
                },
            ])
            .unwrap();
        let mut hnsw = HnswIndex::new();
        hnsw.rebuild_from_store(&store).unwrap();
        let lexical_index = SqliteFtsIndex::new(&store);
        let embed_client = KeywordEmbeddingClient;
        let config = RetrievalConfig::default();
        let pipeline =
            RetrievalPipeline::new(&hnsw, &lexical_index, &store, &embed_client, &config);
        let source_filter = HashSet::from([first.id.clone(), second.id.clone()]);

        let results = pipeline
            .search_source_set("beta", Some(&source_filter))
            .await
            .unwrap();

        assert!(results
            .iter()
            .any(|result| result.chunk_id.0 == "chunk-beta"));
        assert!(results
            .iter()
            .all(|result| source_filter.contains(&result.chunk.source_id)));
        assert!(results
            .iter()
            .all(|result| result.chunk_id.0 != "chunk-outside"));
    }

    #[tokio::test]
    async fn bm25_only_search_skips_query_embedding_and_dense_search() {
        let store = Store::in_memory().unwrap();
        let source = source("src-bm25-only");
        let alpha = insert_child(
            &store,
            &source,
            "chunk-alpha",
            "alpha lexical evidence answers the question",
        );
        let lexical_index = SqliteFtsIndex::new(&store);
        lexical_index.rebuild_from_store(&store).unwrap();
        let vector_index = StaticVectorIndex::new(Vec::new());
        let embed_client = RecordingEmbeddingClient::new();
        let config = RetrievalConfig {
            dense_top_k: 10,
            bm25_top_k: 10,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let pipeline = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &config,
        )
        .with_embedding_enabled(false)
        .require_embedding_profile(&EmbeddingProfileId::default_profile());

        let (results, debug) = pipeline
            .search_source_set_with_debug("alpha question", None)
            .await
            .unwrap();

        assert_eq!(embed_client.call_count(), 0);
        assert_eq!(results[0].chunk_id, alpha.id);
        assert_eq!(debug.dense_vector_path, RetrievalDenseVectorPath::Bm25Only);
        assert_eq!(debug.query_embedding_latency_ms, None);
        assert_eq!(debug.local_spans_ms.query_embedding_ms, 0);
        assert_eq!(debug.local_spans_ms.dense_vector_search_ms, 0);
        assert_eq!(debug.local_spans_ms.response_formatting_ms, 0);
        assert!(debug.dense_hits.is_empty());
        assert!(!debug.bm25_hits.is_empty());
        let encoded = serde_json::to_value(&debug).unwrap();
        assert!(encoded["local_spans_ms"]["bm25_search_ms"].is_u64());
        assert!(encoded["local_spans_ms"]["rrf_fusion_ms"].is_u64());
        assert!(encoded["local_spans_ms"]["final_evidence_pack_ms"].is_u64());
    }

    #[tokio::test]
    async fn prefix_cache_bypass_prefixes_query_embedding_text() {
        let store = Store::in_memory().unwrap();
        let source = source("src-no-cache");
        let alpha = insert_child(&store, &source, "chunk-alpha", "alpha content");
        let vector_index = StaticVectorIndex::new(vec![(alpha.id.clone(), 1.0)]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let embed_client = RecordingQueryEmbeddingClient::new();
        let config = RetrievalConfig {
            dense_top_k: 1,
            bm25_top_k: 1,
            ..RetrievalConfig::default()
        };
        let pipeline = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &config,
        )
        .with_prefix_cache_bypass(true);

        let results = pipeline
            .search_source_set("alpha question", None)
            .await
            .unwrap();

        assert_eq!(results[0].chunk_id, alpha.id);
        let recorded_texts = embed_client.recorded_texts();
        assert_eq!(recorded_texts.len(), 1);
        let query_text = &recorded_texts[0][0];
        assert!(query_text.starts_with('\n'));
        assert_eq!(query_text.trim_start(), "alpha question");
        assert_ne!(query_text, "alpha question");
    }

    #[tokio::test]
    async fn canonical_display_pack_selects_chunk_internal_support_evidence() {
        let store = Store::in_memory().unwrap();
        let source = source("src-canonical-display");
        store.add_source(&source).unwrap();
        let chunk = insert_canonical_chunk(
            &store,
            &source,
            "chunk-2tim4",
            &[
                (
                    "ev-2tim4-1",
                    1,
                    "I solemnly charge you before God and Christ Jesus.",
                ),
                (
                    "ev-2tim4-8",
                    8,
                    "There is reserved for me the alpha crown of righteousness.",
                ),
                ("ev-2tim4-9", 9, "Make every effort to come to me soon."),
            ],
        );
        let vector_index = StaticVectorIndex::new(vec![(chunk.id.clone(), 0.9)]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let embed_client = KeywordEmbeddingClient;
        let config = RetrievalConfig {
            dense_top_k: 1,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };

        let (results, debug) = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &config,
        )
        .search_source_set_with_debug("alpha righteousness", None)
        .await
        .unwrap();

        assert_eq!(chunk_ids(&results), vec!["chunk-2tim4"]);
        assert_eq!(debug.final_evidence_pack.len(), 3);
        assert_eq!(debug.final_evidence_pack[0].evidence_id.0, "ev-2tim4-1");
        assert_eq!(debug.final_evidence_pack[1].evidence_id.0, "ev-2tim4-8");
        assert_eq!(debug.display_evidence_pack.len(), 1);
        let display = &debug.display_evidence_pack[0];
        assert_eq!(display.evidence_id.0, "ev-2tim4-8");
        assert_eq!(display.chunk_id, chunk.id);
        assert_eq!(display.score, results[0].score);
        assert_eq!(display.locator.display, "2 Timothy 4:8");
    }

    #[tokio::test]
    async fn scoped_display_pack_limits_canonical_support_embedding_to_visible_page() {
        let store = Store::in_memory().unwrap();
        let source = source("src-canonical-display-scope");
        store.add_source(&source).unwrap();
        let first = insert_canonical_chunk(
            &store,
            &source,
            "chunk-first",
            &[
                ("ev-first-11", 11, "Opening charge before God."),
                (
                    "ev-first-12",
                    12,
                    "Alpha crown of righteousness is reserved.",
                ),
                ("ev-first-13", 13, "Come to me soon."),
            ],
        );
        let second = insert_canonical_chunk(
            &store,
            &source,
            "chunk-second",
            &[
                ("ev-second-1", 1, "Second opening charge."),
                (
                    "ev-second-8",
                    8,
                    "Second alpha crown is the visible support.",
                ),
                ("ev-second-9", 9, "Second closing request."),
            ],
        );
        let vector_index =
            StaticVectorIndex::new(vec![(first.id.clone(), 0.9), (second.id.clone(), 0.8)]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let retrieval_config = RetrievalConfig {
            dense_top_k: 2,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let rerank_config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "vllm".into(),
            model: "test-reranker".into(),
            top_n: 2,
            ..Default::default()
        };
        let full_client = RecordingQueryEmbeddingClient::new();
        let full_reranker = RecordingReranker::hits(vec![(1, 0.99), (0, 0.7)]);
        let (full_results, full_debug) = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &full_client,
            &retrieval_config,
        )
        .with_reranker(&rerank_config, &full_reranker)
        .search_source_set_with_debug("alpha crown", None)
        .await
        .unwrap();

        let scoped_client = RecordingQueryEmbeddingClient::new();
        let scoped_reranker = RecordingReranker::hits(vec![(1, 0.99), (0, 0.7)]);
        let (scoped_results, scoped_debug) = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &scoped_client,
            &retrieval_config,
        )
        .with_reranker(&rerank_config, &scoped_reranker)
        .search_source_set_with_debug_display_scope(
            "alpha crown",
            None,
            RetrievalDisplayScope::page(1, 1, 1),
        )
        .await
        .unwrap();

        assert_eq!(
            chunk_ids(&full_results),
            vec!["chunk-second", "chunk-first"]
        );
        assert_eq!(chunk_ids(&scoped_results), chunk_ids(&full_results));
        assert_eq!(full_reranker.recorded_docs()[0].len(), 2);
        assert_eq!(scoped_reranker.recorded_docs()[0].len(), 2);
        assert_eq!(full_reranker.recorded_top_ns(), vec![2]);
        assert_eq!(scoped_reranker.recorded_top_ns(), vec![2]);
        assert_eq!(
            scoped_debug.final_evidence_pack.len(),
            full_debug.final_evidence_pack.len()
        );
        assert_eq!(scoped_debug.final_evidence_pack.len(), 6);
        assert_eq!(full_debug.display_evidence_pack.len(), 2);
        assert_eq!(scoped_debug.display_evidence_pack.len(), 2);
        assert_eq!(
            scoped_debug.display_evidence_pack[0].evidence_id,
            full_debug.display_evidence_pack[0].evidence_id
        );
        assert_eq!(
            scoped_debug.display_evidence_pack[0].evidence_id.0,
            "ev-second-8"
        );

        let full_embedding_batches = full_client.recorded_texts();
        let scoped_embedding_batches = scoped_client.recorded_texts();
        assert_eq!(full_embedding_batches.len(), 2);
        assert_eq!(scoped_embedding_batches.len(), 2);
        assert_eq!(full_embedding_batches[0].len(), 1);
        assert_eq!(scoped_embedding_batches[0].len(), 1);
        assert_eq!(full_embedding_batches[1].len(), 6);
        assert_eq!(scoped_embedding_batches[1].len(), 3);
        assert!(scoped_embedding_batches[1]
            .iter()
            .all(|text| text.contains("Second")));
    }

    #[tokio::test]
    async fn compact_debug_skips_full_evidence_pack_without_changing_ranking() {
        let store = Store::in_memory().unwrap();
        let source = source("src-compact-debug");
        store.add_source(&source).unwrap();
        let first = insert_canonical_chunk(
            &store,
            &source,
            "chunk-first",
            &[
                ("ev-first-11", 11, "Opening charge before God."),
                (
                    "ev-first-12",
                    12,
                    "Alpha crown of righteousness is reserved.",
                ),
                ("ev-first-13", 13, "Come to me soon."),
            ],
        );
        let second = insert_canonical_chunk(
            &store,
            &source,
            "chunk-second",
            &[
                ("ev-second-1", 1, "Second opening charge."),
                (
                    "ev-second-8",
                    8,
                    "Second alpha crown is the visible support.",
                ),
                ("ev-second-9", 9, "Second closing request."),
            ],
        );
        let vector_index =
            StaticVectorIndex::new(vec![(first.id.clone(), 0.9), (second.id.clone(), 0.8)]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let retrieval_config = RetrievalConfig {
            dense_top_k: 2,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let rerank_config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "vllm".into(),
            model: "test-reranker".into(),
            top_n: 2,
            ..Default::default()
        };
        let visible_budget = RetrievalCanonicalSelectionBudget::page(1, 1, 1);

        let compact_client = RecordingQueryEmbeddingClient::new();
        let compact_reranker = RecordingReranker::hits(vec![(1, 0.99), (0, 0.7)]);
        let (compact_results, compact_debug) = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &compact_client,
            &retrieval_config,
        )
        .with_reranker(&rerank_config, &compact_reranker)
        .search_source_set_with_debug_options(
            "alpha crown",
            None,
            RetrievalDebugOptions::compact(visible_budget),
        )
        .await
        .unwrap();

        let full_client = RecordingQueryEmbeddingClient::new();
        let full_reranker = RecordingReranker::hits(vec![(1, 0.99), (0, 0.7)]);
        let (full_results, full_debug) = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &full_client,
            &retrieval_config,
        )
        .with_reranker(&rerank_config, &full_reranker)
        .search_source_set_with_debug_options(
            "alpha crown",
            None,
            RetrievalDebugOptions::full(visible_budget),
        )
        .await
        .unwrap();

        assert_eq!(
            chunk_ids(&compact_results),
            vec!["chunk-second", "chunk-first"]
        );
        assert_eq!(chunk_ids(&compact_results), chunk_ids(&full_results));
        assert_eq!(compact_reranker.recorded_docs()[0].len(), 2);
        assert_eq!(
            compact_reranker.recorded_docs()[0],
            full_reranker.recorded_docs()[0]
        );
        assert_eq!(compact_reranker.recorded_top_ns(), vec![2]);
        assert_eq!(
            compact_reranker.recorded_top_ns(),
            full_reranker.recorded_top_ns()
        );

        assert_eq!(
            compact_debug.evidence_pack_mode,
            RetrievalDebugEvidencePackMode::Compact
        );
        assert!(compact_debug.final_evidence_pack.is_empty());
        assert_eq!(compact_debug.final_evidence_count, 6);
        assert_eq!(compact_debug.display_evidence_pack.len(), 2);
        assert_eq!(compact_debug.display_evidence_count, 2);
        assert_eq!(
            compact_debug.display_evidence_pack[0].evidence_id.0,
            "ev-second-8"
        );
        assert_eq!(
            compact_debug.display_evidence_pack[1].evidence_id.0,
            "ev-first-11"
        );
        assert_eq!(compact_debug.local_spans_ms.final_evidence_pack_ms, 0);

        assert_eq!(
            full_debug.evidence_pack_mode,
            RetrievalDebugEvidencePackMode::Full
        );
        assert_eq!(full_debug.final_evidence_pack.len(), 6);
        assert_eq!(full_debug.final_evidence_count, 6);
        assert_eq!(full_debug.display_evidence_pack.len(), 2);
        assert_eq!(
            full_debug.display_evidence_pack[0].evidence_id.0,
            "ev-second-8"
        );

        let compact_embedding_batches = compact_client.recorded_texts();
        assert_eq!(compact_embedding_batches.len(), 2);
        assert_eq!(compact_embedding_batches[0].len(), 1);
        assert_eq!(compact_embedding_batches[1].len(), 3);
        assert!(compact_embedding_batches[1]
            .iter()
            .all(|text| text.contains("Second")));
    }

    #[tokio::test]
    async fn canonical_support_and_display_budgets_are_independent() {
        let store = Store::in_memory().unwrap();
        let source = source("src-canonical-independent-budgets");
        store.add_source(&source).unwrap();
        let first = insert_canonical_chunk(
            &store,
            &source,
            "chunk-first",
            &[
                ("ev-first-11", 11, "Opening charge before God."),
                (
                    "ev-first-12",
                    12,
                    "Alpha crown of righteousness is reserved.",
                ),
                ("ev-first-13", 13, "Come to me soon."),
            ],
        );
        let second = insert_canonical_chunk(
            &store,
            &source,
            "chunk-second",
            &[
                ("ev-second-1", 1, "Second opening charge."),
                (
                    "ev-second-8",
                    8,
                    "Second alpha crown is the visible support.",
                ),
                ("ev-second-9", 9, "Second closing request."),
            ],
        );
        let vector_index =
            StaticVectorIndex::new(vec![(first.id.clone(), 0.9), (second.id.clone(), 0.8)]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let retrieval_config = RetrievalConfig {
            dense_top_k: 2,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let rerank_config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "vllm".into(),
            model: "test-reranker".into(),
            top_n: 2,
            ..Default::default()
        };
        let visible_scope = RetrievalDisplayScope::page(1, 1, 1);

        let support_limited_client = RecordingQueryEmbeddingClient::new();
        let support_limited_reranker = RecordingReranker::hits(vec![(1, 0.99), (0, 0.7)]);
        let (support_limited_results, support_limited_debug) = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &support_limited_client,
            &retrieval_config,
        )
        .with_reranker(&rerank_config, &support_limited_reranker)
        .search_source_set_with_debug_canonical_budget(
            "alpha crown",
            None,
            RetrievalCanonicalSelectionBudget::new(visible_scope, RetrievalDisplayScope::All),
        )
        .await
        .unwrap();

        assert_eq!(
            chunk_ids(&support_limited_results),
            vec!["chunk-second", "chunk-first"]
        );
        assert_eq!(support_limited_reranker.recorded_docs()[0].len(), 2);
        assert_eq!(support_limited_reranker.recorded_top_ns(), vec![2]);
        assert_eq!(support_limited_debug.final_evidence_pack.len(), 6);
        assert_eq!(support_limited_debug.display_evidence_pack.len(), 2);
        assert_eq!(
            support_limited_debug.display_evidence_pack[0].evidence_id.0,
            "ev-second-8"
        );
        assert_eq!(
            support_limited_debug.display_evidence_pack[1].evidence_id.0,
            "ev-first-11"
        );
        let support_limited_batches = support_limited_client.recorded_texts();
        assert_eq!(support_limited_batches.len(), 2);
        assert_eq!(support_limited_batches[0].len(), 1);
        assert_eq!(support_limited_batches[1].len(), 3);
        assert!(support_limited_batches[1]
            .iter()
            .all(|text| text.contains("Second")));

        let display_limited_client = RecordingQueryEmbeddingClient::new();
        let display_limited_reranker = RecordingReranker::hits(vec![(1, 0.99), (0, 0.7)]);
        let (display_limited_results, display_limited_debug) = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &display_limited_client,
            &retrieval_config,
        )
        .with_reranker(&rerank_config, &display_limited_reranker)
        .search_source_set_with_debug_canonical_budget(
            "alpha crown",
            None,
            RetrievalCanonicalSelectionBudget::new(RetrievalDisplayScope::All, visible_scope),
        )
        .await
        .unwrap();

        assert_eq!(
            chunk_ids(&display_limited_results),
            chunk_ids(&support_limited_results)
        );
        assert_eq!(display_limited_reranker.recorded_docs()[0].len(), 2);
        assert_eq!(display_limited_reranker.recorded_top_ns(), vec![2]);
        assert_eq!(display_limited_debug.final_evidence_pack.len(), 6);
        assert_eq!(display_limited_debug.display_evidence_pack.len(), 2);
        assert_eq!(
            display_limited_debug.display_evidence_pack[0].evidence_id.0,
            "ev-second-8"
        );
        assert_eq!(
            display_limited_debug.display_evidence_pack[1].evidence_id.0,
            "ev-first-11"
        );
        let display_limited_batches = display_limited_client.recorded_texts();
        assert_eq!(display_limited_batches.len(), 2);
        assert_eq!(display_limited_batches[0].len(), 1);
        assert_eq!(display_limited_batches[1].len(), 6);
    }

    #[tokio::test]
    async fn canonical_detail_by_evidence_id_or_locator_scores_only_target_result() {
        let store = Store::in_memory().unwrap();
        let source = source("src-canonical-detail");
        store.add_source(&source).unwrap();
        let first = insert_canonical_chunk(
            &store,
            &source,
            "chunk-first",
            &[
                ("ev-first-11", 11, "Opening charge before God."),
                (
                    "ev-first-12",
                    12,
                    "Alpha crown of righteousness is reserved.",
                ),
                ("ev-first-13", 13, "Come to me soon."),
            ],
        );
        let second = insert_canonical_chunk(
            &store,
            &source,
            "chunk-second",
            &[
                ("ev-second-1", 1, "Second opening charge."),
                (
                    "ev-second-8",
                    8,
                    "Second alpha crown is the visible support.",
                ),
                ("ev-second-9", 9, "Second closing request."),
            ],
        );
        let vector_index =
            StaticVectorIndex::new(vec![(first.id.clone(), 0.9), (second.id.clone(), 0.8)]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let retrieval_config = RetrievalConfig {
            dense_top_k: 2,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let rerank_config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "vllm".into(),
            model: "test-reranker".into(),
            top_n: 2,
            ..Default::default()
        };
        let client = RecordingQueryEmbeddingClient::new();
        let reranker = RecordingReranker::hits(vec![(1, 0.99), (0, 0.7)]);
        let pipeline = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &client,
            &retrieval_config,
        )
        .with_reranker(&rerank_config, &reranker);
        let (results, debug) = pipeline
            .search_source_set_with_debug_canonical_budget(
                "alpha crown",
                None,
                RetrievalCanonicalSelectionBudget::page(1, 1, 1),
            )
            .await
            .unwrap();

        assert_eq!(chunk_ids(&results), vec!["chunk-second", "chunk-first"]);
        assert_eq!(debug.final_evidence_pack.len(), 6);
        assert_eq!(debug.display_evidence_pack.len(), 2);
        assert_eq!(debug.display_evidence_pack[0].evidence_id.0, "ev-second-8");
        assert_eq!(debug.display_evidence_pack[1].evidence_id.0, "ev-first-11");
        assert_eq!(reranker.recorded_docs()[0].len(), 2);
        assert_eq!(reranker.recorded_top_ns(), vec![2]);
        let initial_batches = client.recorded_texts();
        assert_eq!(initial_batches.len(), 2);
        assert_eq!(initial_batches[1].len(), 3);
        assert!(initial_batches[1]
            .iter()
            .all(|text| text.contains("Second")));

        let target_id = EvidenceId("ev-first-13".into());
        let detail_by_id = pipeline
            .canonical_detail_evidence_pack_debug(
                "alpha crown",
                &results,
                RetrievalCanonicalDetailTarget::EvidenceId(&target_id),
            )
            .await
            .expect("target evidence id should resolve to canonical detail");

        assert_eq!(detail_by_id.display_entry.evidence_id.0, "ev-first-12");
        assert_eq!(detail_by_id.support_evidence_pack.len(), 3);
        assert!(detail_by_id
            .support_evidence_pack
            .iter()
            .any(|entry| entry.evidence_id.0 == "ev-first-13"));
        let detail_batches = client.recorded_texts();
        assert_eq!(detail_batches.len(), 4);
        assert_eq!(detail_batches[2].len(), 1);
        assert_eq!(detail_batches[3].len(), 3);
        assert!(detail_batches[3]
            .iter()
            .all(|text| !text.contains("Second")));
        assert!(detail_batches[3]
            .iter()
            .any(|text| text.contains("Alpha crown")));

        let target_locator = results[1]
            .evidence_units
            .iter()
            .find(|evidence| evidence.id.0 == "ev-first-13")
            .expect("target locator")
            .locator
            .clone();
        let detail_by_locator = pipeline
            .canonical_detail_evidence_pack_debug(
                "alpha crown",
                &results,
                RetrievalCanonicalDetailTarget::Locator(&target_locator),
            )
            .await
            .expect("target locator should resolve to canonical detail");

        assert_eq!(detail_by_locator.display_entry.evidence_id.0, "ev-first-12");
        assert_eq!(
            detail_by_locator.support_evidence_pack,
            detail_by_id.support_evidence_pack
        );
    }

    #[tokio::test]
    async fn compact_canonical_regression_bounds_many_expanded_evidence_units() {
        const RANKED_CHUNK_COUNT: usize = 4;
        const EVIDENCE_PER_CHUNK: usize = 40;
        const EXPANDED_EVIDENCE_COUNT: usize = RANKED_CHUNK_COUNT * EVIDENCE_PER_CHUNK;
        const VISIBLE_SUPPORT_OFFSET: usize = 16;
        const DETAIL_SUPPORT_OFFSET: usize = 18;
        const DETAIL_TARGET_OFFSET: usize = 31;

        let store = Store::in_memory().unwrap();
        let source = source("src-canonical-many-expanded");
        store.add_source(&source).unwrap();
        let anchor = insert_numbered_canonical_chunk(
            &store,
            &source,
            "chunk-anchor",
            "ev-anchor",
            1,
            EVIDENCE_PER_CHUNK,
            7,
            "anchor",
        );
        let visible = insert_numbered_canonical_chunk(
            &store,
            &source,
            "chunk-visible",
            "ev-visible",
            101,
            EVIDENCE_PER_CHUNK,
            VISIBLE_SUPPORT_OFFSET,
            "visible",
        );
        let detail = insert_numbered_canonical_chunk(
            &store,
            &source,
            "chunk-detail",
            "ev-detail",
            201,
            EVIDENCE_PER_CHUNK,
            DETAIL_SUPPORT_OFFSET,
            "detail",
        );
        let tail = insert_numbered_canonical_chunk(
            &store,
            &source,
            "chunk-tail",
            "ev-tail",
            301,
            EVIDENCE_PER_CHUNK,
            23,
            "tail",
        );
        let vector_index = StaticVectorIndex::new(vec![
            (anchor.id.clone(), 0.95),
            (visible.id.clone(), 0.9),
            (detail.id.clone(), 0.85),
            (tail.id.clone(), 0.8),
        ]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let retrieval_config = RetrievalConfig {
            dense_top_k: RANKED_CHUNK_COUNT,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let rerank_config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "vllm".into(),
            model: "test-reranker".into(),
            top_n: RANKED_CHUNK_COUNT,
            ..Default::default()
        };
        let visible_budget = RetrievalCanonicalSelectionBudget::page(1, 1, 1);
        let expected_order = vec![
            "chunk-visible",
            "chunk-detail",
            "chunk-anchor",
            "chunk-tail",
        ];
        let visible_support_id =
            numbered_canonical_evidence_id("ev-visible", 101, VISIBLE_SUPPORT_OFFSET);
        let detail_support_id =
            numbered_canonical_evidence_id("ev-detail", 201, DETAIL_SUPPORT_OFFSET);
        let detail_target_id = EvidenceId(numbered_canonical_evidence_id(
            "ev-detail",
            201,
            DETAIL_TARGET_OFFSET,
        ));

        let compact_client = RecordingQueryEmbeddingClient::new();
        let compact_reranker =
            RecordingReranker::hits(vec![(1, 0.99), (2, 0.95), (0, 0.8), (3, 0.7)]);
        let compact_pipeline = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &compact_client,
            &retrieval_config,
        )
        .with_reranker(&rerank_config, &compact_reranker);
        let (compact_results, compact_debug) = compact_pipeline
            .search_source_set_with_debug_options(
                "alpha crown",
                None,
                RetrievalDebugOptions::compact(visible_budget),
            )
            .await
            .unwrap();

        assert_eq!(chunk_ids(&compact_results), expected_order);
        assert_eq!(compact_reranker.call_count(), 1);
        assert_eq!(
            compact_reranker.recorded_docs()[0].len(),
            RANKED_CHUNK_COUNT
        );
        assert_eq!(compact_reranker.recorded_top_ns(), vec![RANKED_CHUNK_COUNT]);
        assert_eq!(
            compact_debug.evidence_pack_mode,
            RetrievalDebugEvidencePackMode::Compact
        );
        assert!(compact_debug.final_evidence_pack.is_empty());
        assert_eq!(compact_debug.final_evidence_count, EXPANDED_EVIDENCE_COUNT);
        assert_eq!(compact_debug.display_evidence_count, RANKED_CHUNK_COUNT);
        assert_eq!(
            compact_debug.display_evidence_pack.len(),
            RANKED_CHUNK_COUNT
        );
        assert_eq!(
            compact_debug.display_evidence_pack[0].evidence_id.0,
            visible_support_id
        );
        assert_eq!(
            compact_debug.display_evidence_pack[1].evidence_id.0,
            "ev-detail-201"
        );
        assert_eq!(compact_debug.local_spans_ms.final_evidence_pack_ms, 0);

        let compact_embedding_batches = compact_client.recorded_texts();
        assert_eq!(compact_embedding_batches.len(), 2);
        assert_eq!(compact_embedding_batches[0].len(), 1);
        assert_eq!(compact_embedding_batches[1].len(), EVIDENCE_PER_CHUNK);
        assert!(compact_embedding_batches[1]
            .iter()
            .all(|text| text.contains("visible")));
        assert!(compact_embedding_batches
            .iter()
            .all(|batch| batch.len() < EXPANDED_EVIDENCE_COUNT));

        let full_client = RecordingQueryEmbeddingClient::new();
        let full_reranker = RecordingReranker::hits(vec![(1, 0.99), (2, 0.95), (0, 0.8), (3, 0.7)]);
        let (full_results, full_debug) = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &full_client,
            &retrieval_config,
        )
        .with_reranker(&rerank_config, &full_reranker)
        .search_source_set_with_debug_options(
            "alpha crown",
            None,
            RetrievalDebugOptions::full(visible_budget),
        )
        .await
        .unwrap();

        assert_eq!(chunk_ids(&full_results), chunk_ids(&compact_results));
        assert_eq!(
            full_debug.evidence_pack_mode,
            RetrievalDebugEvidencePackMode::Full
        );
        assert_eq!(
            full_debug.final_evidence_pack.len(),
            EXPANDED_EVIDENCE_COUNT
        );
        assert_eq!(full_debug.final_evidence_count, EXPANDED_EVIDENCE_COUNT);
        assert_eq!(full_debug.display_evidence_count, RANKED_CHUNK_COUNT);
        assert_eq!(
            full_debug.display_evidence_pack[0].evidence_id.0,
            visible_support_id
        );
        assert!(full_debug
            .final_evidence_pack
            .iter()
            .any(|entry| entry.evidence_id == detail_target_id));

        let target_locator = compact_results[1]
            .evidence_units
            .iter()
            .find(|evidence| evidence.id == detail_target_id)
            .expect("detail target evidence")
            .locator
            .clone();
        let detail_by_id = compact_pipeline
            .canonical_detail_evidence_pack_debug(
                "alpha crown",
                &compact_results,
                RetrievalCanonicalDetailTarget::EvidenceId(&detail_target_id),
            )
            .await
            .expect("non-visible target evidence id should resolve");

        assert_eq!(compact_reranker.call_count(), 1);
        assert_eq!(detail_by_id.display_entry.evidence_id.0, detail_support_id);
        assert_eq!(detail_by_id.support_evidence_pack.len(), EVIDENCE_PER_CHUNK);
        assert!(detail_by_id
            .support_evidence_pack
            .iter()
            .any(|entry| entry.evidence_id == detail_target_id));
        let detail_embedding_batches = compact_client.recorded_texts();
        assert_eq!(detail_embedding_batches.len(), 4);
        assert_eq!(detail_embedding_batches[2].len(), 1);
        assert_eq!(detail_embedding_batches[3].len(), EVIDENCE_PER_CHUNK);
        assert!(detail_embedding_batches[3]
            .iter()
            .all(|text| text.contains("detail")));

        let detail_by_locator = compact_pipeline
            .canonical_detail_evidence_pack_debug(
                "alpha crown",
                &compact_results,
                RetrievalCanonicalDetailTarget::Locator(&target_locator),
            )
            .await
            .expect("non-visible target locator should resolve");

        assert_eq!(compact_reranker.call_count(), 1);
        assert_eq!(
            detail_by_locator.support_evidence_pack,
            detail_by_id.support_evidence_pack
        );
        assert_eq!(
            detail_by_locator.display_entry.evidence_id,
            detail_by_id.display_entry.evidence_id
        );
    }

    #[tokio::test]
    async fn low_memory_dense_search_reads_stored_vectors_without_resident_hnsw() {
        let store = Store::in_memory().unwrap();
        let source = source("src-low-memory");
        store.add_source(&source).unwrap();
        let alpha = insert_text_chunk(&store, &source, "chunk-alpha", "alpha semantic evidence");
        let beta = insert_text_chunk(&store, &source, "chunk-beta", "beta semantic evidence");
        store
            .replace_all_vector_documents(&[
                VectorDocument {
                    chunk_id: alpha.id.clone(),
                    source_id: source.id.clone(),
                    vector: keyword_vector(&alpha.text),
                },
                VectorDocument {
                    chunk_id: beta.id.clone(),
                    source_id: source.id.clone(),
                    vector: keyword_vector(&beta.text),
                },
            ])
            .unwrap();
        let empty_hnsw = StaticVectorIndex::new(Vec::new());
        let lexical_index = SqliteFtsIndex::new(&store);
        lexical_index.rebuild_from_store(&store).unwrap();
        let embed_client = KeywordEmbeddingClient;
        let config = RetrievalConfig {
            dense_top_k: 2,
            bm25_top_k: 1,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let pipeline =
            RetrievalPipeline::new(&empty_hnsw, &lexical_index, &store, &embed_client, &config)
                .with_vector_residency(VectorIndexResidency::LowMemory)
                .require_embedding_profile(&EmbeddingProfileId::default_profile());

        let (results, debug) = pipeline
            .search_source_set_with_debug("alpha", None)
            .await
            .unwrap();

        assert_eq!(
            debug.dense_vector_path,
            RetrievalDenseVectorPath::LowMemorySqliteScan
        );
        assert_eq!(debug.dense_hits[0].chunk_id, alpha.id);
        assert!(results.iter().any(|result| result.chunk_id == alpha.id));
    }

    #[tokio::test]
    async fn resident_hnsw_dense_search_uses_resident_index() {
        let store = Store::in_memory().unwrap();
        let source = source("src-resident-hnsw");
        store.add_source(&source).unwrap();
        let alpha = insert_text_chunk(&store, &source, "chunk-alpha", "alpha semantic evidence");
        let beta = insert_text_chunk(&store, &source, "chunk-beta", "beta semantic evidence");
        store
            .replace_all_vector_documents(&[
                VectorDocument {
                    chunk_id: alpha.id.clone(),
                    source_id: source.id.clone(),
                    vector: keyword_vector(&alpha.text),
                },
                VectorDocument {
                    chunk_id: beta.id.clone(),
                    source_id: source.id.clone(),
                    vector: keyword_vector(&beta.text),
                },
            ])
            .unwrap();
        let mut hnsw = HnswIndex::new();
        hnsw.rebuild_from_store(&store).unwrap();
        let lexical_index = SqliteFtsIndex::new(&store);
        lexical_index.rebuild_from_store(&store).unwrap();
        let embed_client = KeywordEmbeddingClient;
        let config = RetrievalConfig {
            dense_top_k: 2,
            bm25_top_k: 1,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let pipeline =
            RetrievalPipeline::new(&hnsw, &lexical_index, &store, &embed_client, &config)
                .with_vector_residency(VectorIndexResidency::ResidentHnsw);

        let (results, debug) = pipeline
            .search_source_set_with_debug("alpha", None)
            .await
            .unwrap();

        assert_eq!(
            debug.dense_vector_path,
            RetrievalDenseVectorPath::ResidentHnsw
        );
        assert_eq!(debug.dense_hits[0].chunk_id, alpha.id);
        assert!(results.iter().any(|result| result.chunk_id == alpha.id));
    }

    include!("retrieve/vector_search_resource_tests.rs");

    #[tokio::test]
    async fn requested_profile_without_vectors_fails_clearly_before_bm25_fallback() {
        let store = Store::in_memory().unwrap();
        let source = source("src-missing-profile");
        insert_child(&store, &source, "chunk-alpha", "alpha content");
        let vector_index = StaticVectorIndex::new(Vec::new());
        let lexical_index = SqliteFtsIndex::new(&store);
        let embed_client = KeywordEmbeddingClient;
        let config = RetrievalConfig {
            dense_top_k: 1,
            bm25_top_k: 10,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let profile_id = EmbeddingProfileId::new("alt").unwrap();
        let pipeline = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &config,
        )
        .require_embedding_profile(&profile_id);

        let error = pipeline.search("alpha").await.unwrap_err();

        assert!(error
            .to_string()
            .contains("embedding profile 'alt' has no vectors"));
        assert!(error.to_string().contains("--vectors-only"));
    }

    #[cfg(feature = "qdrant")]
    #[tokio::test]
    async fn qdrant_remote_hits_are_ignored_when_local_profile_has_no_vectors() {
        let (qdrant_url, handle) = spawn_optional_qdrant_search_response(
            200,
            r#"{"status":"ok","result":[{"score":0.99,"payload":{"chunk_id":"chunk-local-stale-profile"}}]}"#,
        );
        let store = Store::in_memory().unwrap();
        let source = source("src-qdrant-reset-profile");
        insert_child(
            &store,
            &source,
            "chunk-local-stale-profile",
            "alpha content",
        );
        let vector_index = StaticVectorIndex::new(Vec::new());
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let embed_client = KeywordEmbeddingClient;
        let config = RetrievalConfig {
            dense_top_k: 1,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let qdrant = qdrant_config(qdrant_url);
        let pipeline = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &config,
        )
        .with_qdrant_search(&qdrant);

        let results = pipeline
            .search_filtered("alpha", Some(&source.id))
            .await
            .unwrap();

        assert!(results.is_empty());
        assert_eq!(
            handle.join().unwrap().as_deref(),
            Some("POST /collections/verbatim/points/search HTTP/1.1")
        );
    }

    #[cfg(feature = "qdrant")]
    #[tokio::test]
    async fn qdrant_search_failure_falls_back_to_local_dense_index() {
        let (qdrant_url, handle) =
            spawn_qdrant_search_response(500, r#"{"status":{"error":"down"}}"#);
        let store = Store::in_memory().unwrap();
        let source = source("src-qdrant-fallback");
        let chunk = insert_child(&store, &source, "chunk-local", "alpha content");
        let vector_index = StaticVectorIndex::new(vec![(chunk.id.clone(), 0.9)]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let embed_client = KeywordEmbeddingClient;
        let config = RetrievalConfig {
            dense_top_k: 1,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let qdrant = qdrant_config(qdrant_url);
        let pipeline = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &config,
        )
        .with_qdrant_search(&qdrant);

        let results = pipeline
            .search_filtered("alpha", Some(&source.id))
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_id.0, "chunk-local");
        assert_eq!(
            handle.join().unwrap(),
            "POST /collections/verbatim/points/search HTTP/1.1"
        );
    }

    #[cfg(feature = "qdrant")]
    #[tokio::test]
    async fn qdrant_empty_success_keeps_local_fallback_bounded_before_source_filter() {
        let (qdrant_url, handle) =
            spawn_qdrant_search_response(200, r#"{"status":"ok","result":[]}"#);
        let store = Store::in_memory().unwrap();
        let wanted_source = source("src-qdrant-empty");
        let other_source = source("src-qdrant-other");
        let wanted_chunk = insert_child(&store, &wanted_source, "chunk-wanted", "alpha wanted");
        let other_chunk = insert_child(&store, &other_source, "chunk-other", "alpha other");
        let vector_index = StaticVectorIndex::new(vec![
            (other_chunk.id.clone(), 0.95),
            (wanted_chunk.id.clone(), 0.9),
        ]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let embed_client = KeywordEmbeddingClient;
        let config = RetrievalConfig {
            dense_top_k: 1,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let qdrant = qdrant_config(qdrant_url);
        let pipeline = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &config,
        )
        .with_qdrant_search(&qdrant);

        let results = pipeline
            .search_filtered("alpha", Some(&wanted_source.id))
            .await
            .unwrap();

        assert!(results.is_empty());
        assert_eq!(
            handle.join().unwrap(),
            "POST /collections/verbatim/points/search HTTP/1.1"
        );
    }

    #[cfg(feature = "qdrant")]
    #[tokio::test]
    async fn qdrant_valid_success_prefers_remote_then_fills_from_local_dense_index() {
        let (qdrant_url, handle) = spawn_qdrant_search_response(
            200,
            r#"{"status":"ok","result":[{"id":"749ce13a-d809-57fe-a274-b32bec2735f0","score":0.99,"payload":{"chunk_id":"chunk-remote-preferred","profile_generation":1,"profile_id":"default","source_id":"src-qdrant-preferred"}}]}"#,
        );
        let store = Store::in_memory().unwrap();
        let source = source("src-qdrant-preferred");
        store.add_source(&source).unwrap();
        let remote_preferred = insert_text_chunk(
            &store,
            &source,
            "chunk-remote-preferred",
            "alpha remote preferred",
        );
        let local_fallback = insert_text_chunk(
            &store,
            &source,
            "chunk-local-fallback",
            "alpha local fallback",
        );
        store
            .replace_all_vector_documents(&[
                VectorDocument {
                    chunk_id: remote_preferred.id.clone(),
                    source_id: source.id.clone(),
                    vector: keyword_vector(&remote_preferred.text),
                },
                VectorDocument {
                    chunk_id: local_fallback.id.clone(),
                    source_id: source.id.clone(),
                    vector: keyword_vector(&local_fallback.text),
                },
            ])
            .unwrap();
        let vector_index = StaticVectorIndex::new(vec![(local_fallback.id.clone(), 0.95)]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let embed_client = KeywordEmbeddingClient;
        let config = RetrievalConfig {
            dense_top_k: 2,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let qdrant = qdrant_config(qdrant_url);
        let pipeline = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &config,
        )
        .with_qdrant_search(&qdrant);

        let mut source_filter = HashSet::new();
        source_filter.insert(source.id.clone());
        let (results, debug) = pipeline
            .search_source_set_with_debug("alpha", Some(&source_filter))
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].chunk_id, remote_preferred.id);
        assert_eq!(results[1].chunk_id, local_fallback.id);
        assert_eq!(debug.dense_vector_path, RetrievalDenseVectorPath::Qdrant);
        assert_eq!(
            handle.join().unwrap(),
            "POST /collections/verbatim/points/search HTTP/1.1"
        );
    }

    #[cfg(feature = "qdrant")]
    #[tokio::test]
    async fn qdrant_stale_generation_hits_fall_back_to_local_dense_evidence() {
        let (qdrant_url, handle) = spawn_qdrant_search_response(
            200,
            r#"{"status":"ok","result":[{"id":"918fef15-a37d-57b3-99be-b55276fac69c","score":0.99,"payload":{"chunk_id":"src-qdrant-stale-existing-child-0","profile_generation":0,"profile_id":"default","source_id":"src-qdrant-stale-existing"}},{"id":"23bbeb3b-94ef-54a8-96bf-2706e020f9c4","score":0.98,"payload":{"chunk_id":"src-qdrant-stale-existing-child-2","profile_generation":0,"profile_id":"default","source_id":"src-qdrant-stale-existing"}}]}"#,
        );
        let store = Store::in_memory().unwrap();
        let source = source("src-qdrant-stale-existing");
        store.add_source(&source).unwrap();
        let stale_remote_best = insert_text_chunk(
            &store,
            &source,
            "src-qdrant-stale-existing-child-0",
            "alpha stale remote",
        );
        let current_local_best = insert_text_chunk(
            &store,
            &source,
            "src-qdrant-stale-existing-child-1",
            "alpha current local",
        );
        let remote_only_stale = insert_text_chunk(
            &store,
            &source,
            "src-qdrant-stale-existing-child-2",
            "alpha remote only stale",
        );
        store
            .replace_all_vector_documents(&[
                VectorDocument {
                    chunk_id: stale_remote_best.id.clone(),
                    source_id: source.id.clone(),
                    vector: keyword_vector(&stale_remote_best.text),
                },
                VectorDocument {
                    chunk_id: current_local_best.id.clone(),
                    source_id: source.id.clone(),
                    vector: keyword_vector(&current_local_best.text),
                },
            ])
            .unwrap();
        let vector_index = StaticVectorIndex::new(vec![
            (current_local_best.id.clone(), 0.95),
            (stale_remote_best.id.clone(), 0.5),
            (remote_only_stale.id.clone(), 0.4),
        ]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let embed_client = KeywordEmbeddingClient;
        let config = RetrievalConfig {
            dense_top_k: 2,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let qdrant = qdrant_config(qdrant_url);
        let pipeline = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &config,
        )
        .with_qdrant_search(&qdrant);

        let results = pipeline
            .search_filtered("alpha", Some(&source.id))
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].chunk_id, current_local_best.id);
        assert_eq!(results[1].chunk_id, stale_remote_best.id);
        assert!(!results
            .iter()
            .any(|result| result.chunk_id == remote_only_stale.id));
        assert_eq!(
            handle.join().unwrap(),
            "POST /collections/verbatim/points/search HTTP/1.1"
        );
    }

    #[cfg(feature = "qdrant")]
    #[tokio::test]
    async fn qdrant_stale_generation_same_chunk_hit_does_not_block_local_fallback() {
        let (qdrant_url, handle) = spawn_qdrant_search_response(
            200,
            r#"{"status":"ok","result":[{"id":"bf1f241d-a71d-51ec-a96e-a1919186382c","score":0.99,"payload":{"chunk_id":"chunk-rebuilt-same-id","profile_generation":1,"profile_id":"default","source_id":"src-qdrant-rebuilt-same-id"}}]}"#,
        );
        let store = Store::in_memory().unwrap();
        let source = source("src-qdrant-rebuilt-same-id");
        store.add_source(&source).unwrap();
        let rebuilt_same_id = insert_text_chunk(
            &store,
            &source,
            "chunk-rebuilt-same-id",
            "alpha rebuilt same id",
        );
        let fresh_best = insert_text_chunk(
            &store,
            &source,
            "chunk-fresh-current-generation",
            "alpha fresh current generation",
        );
        store
            .replace_all_vector_documents(&[VectorDocument {
                chunk_id: rebuilt_same_id.id.clone(),
                source_id: source.id.clone(),
                vector: keyword_vector(&rebuilt_same_id.text),
            }])
            .unwrap();
        assert_eq!(store.index_generation().unwrap(), 1);
        store
            .replace_all_vector_documents(&[
                VectorDocument {
                    chunk_id: rebuilt_same_id.id.clone(),
                    source_id: source.id.clone(),
                    vector: keyword_vector(&rebuilt_same_id.text),
                },
                VectorDocument {
                    chunk_id: fresh_best.id.clone(),
                    source_id: source.id.clone(),
                    vector: keyword_vector(&fresh_best.text),
                },
            ])
            .unwrap();
        assert_eq!(store.index_generation().unwrap(), 2);
        let vector_index = StaticVectorIndex::new(vec![
            (fresh_best.id.clone(), 0.95),
            (rebuilt_same_id.id.clone(), 0.5),
        ]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let embed_client = KeywordEmbeddingClient;
        let config = RetrievalConfig {
            dense_top_k: 2,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let qdrant = qdrant_config(qdrant_url);
        let pipeline = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &config,
        )
        .with_qdrant_search(&qdrant);

        let results = pipeline
            .search_filtered("alpha", Some(&source.id))
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].chunk_id, fresh_best.id);
        assert_eq!(results[1].chunk_id, rebuilt_same_id.id);
        assert_eq!(
            handle.join().unwrap(),
            "POST /collections/verbatim/points/search HTTP/1.1"
        );
    }

    #[cfg(feature = "qdrant")]
    #[tokio::test]
    async fn qdrant_stale_or_malformed_success_fills_from_local_dense_index() {
        let (qdrant_url, handle) = spawn_qdrant_search_response(
            200,
            r#"{"status":"ok","result":[{"score":0.99,"payload":{"chunk_id":"missing-chunk"}},{"score":0.98,"payload":{}}]}"#,
        );
        let store = Store::in_memory().unwrap();
        let source = source("src-qdrant-stale");
        let chunk = insert_child(&store, &source, "chunk-local", "alpha content");
        let vector_index = StaticVectorIndex::new(vec![(chunk.id.clone(), 0.9)]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let embed_client = KeywordEmbeddingClient;
        let config = RetrievalConfig {
            dense_top_k: 1,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let qdrant = qdrant_config(qdrant_url);
        let pipeline = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &config,
        )
        .with_qdrant_search(&qdrant);

        let results = pipeline.search("alpha").await.unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_id, chunk.id);
        assert_eq!(
            handle.join().unwrap(),
            "POST /collections/verbatim/points/search HTTP/1.1"
        );
    }

    #[tokio::test]
    async fn rerank_success_replaces_rrf_order_before_graph_expansion() {
        let store = Store::in_memory().unwrap();
        let source = source("src-rerank");
        store.add_source(&source).unwrap();
        let first = insert_text_chunk(&store, &source, "chunk-first", "alpha first");
        let second = insert_text_chunk(&store, &source, "chunk-second", "alpha second");
        let third = insert_text_chunk(&store, &source, "chunk-third", "alpha third");
        let vector_index = StaticVectorIndex::new(vec![
            (first.id.clone(), 0.9),
            (second.id.clone(), 0.8),
            (third.id.clone(), 0.7),
        ]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let embed_client = KeywordEmbeddingClient;
        let retrieval_config = RetrievalConfig {
            dense_top_k: 3,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let rerank_config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "vllm".into(),
            model: "test-reranker".into(),
            top_n: 2,
            ..Default::default()
        };
        let reranker = RecordingReranker::hits(vec![(2, 0.99), (0, 0.7)]);

        let (results, debug) = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
        )
        .with_reranker(&rerank_config, &reranker)
        .search_filtered_with_debug("alpha", None)
        .await
        .unwrap();

        assert_eq!(chunk_ids(&results), vec!["chunk-third", "chunk-first"]);
        assert_eq!(reranker.call_count(), 1);
        assert_eq!(reranker.recorded_top_ns(), vec![2]);
        assert_eq!(reranker.recorded_docs()[0].len(), 3);
        assert_eq!(debug.reranker.status, RetrievalRerankStatus::Succeeded);
        assert_eq!(debug.reranker.provider.as_deref(), Some("vllm"));
        assert_eq!(debug.reranker.model.as_deref(), Some("test-reranker"));
        assert_eq!(debug.reranker.top_n, Some(2));
        assert_eq!(debug.reranker.candidate_count, Some(3));
        assert_eq!(debug.reranker.scores[0].chunk_id, third.id);
        assert_eq!(debug.final_evidence_pack[0].chunk_id, third.id);
    }

    #[tokio::test]
    async fn rerank_documents_include_context_and_chunk_text() {
        let store = Store::in_memory().unwrap();
        let source = source("src-rerank-context");
        store.add_source(&source).unwrap();
        let chunk = insert_text_chunk_with_context(
            &store,
            &source,
            "chunk-context",
            "the original passage contains the decisive evidence",
            "generated document context",
        );
        let vector_index = StaticVectorIndex::new(vec![(chunk.id.clone(), 0.9)]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let embed_client = KeywordEmbeddingClient;
        let retrieval_config = RetrievalConfig {
            dense_top_k: 1,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let rerank_config = RerankConfig {
            enabled: true,
            top_n: 1,
            ..Default::default()
        };
        let reranker = RecordingReranker::hits(vec![(0, 0.9)]);

        let (_results, _debug) = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
        )
        .with_reranker(&rerank_config, &reranker)
        .search_filtered_with_debug("alpha", None)
        .await
        .unwrap();

        assert_eq!(
            reranker.recorded_docs(),
            vec![vec![
                "generated document context the original passage contains the decisive evidence"
                    .to_string()
            ]]
        );
    }

    #[tokio::test]
    async fn rerank_disabled_preserves_rrf_order_and_does_not_call_reranker() {
        let store = Store::in_memory().unwrap();
        let source = source("src-rerank-disabled");
        store.add_source(&source).unwrap();
        let first = insert_text_chunk(&store, &source, "chunk-first", "alpha first");
        let second = insert_text_chunk(&store, &source, "chunk-second", "alpha second");
        let vector_index =
            StaticVectorIndex::new(vec![(first.id.clone(), 0.9), (second.id.clone(), 0.8)]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let embed_client = KeywordEmbeddingClient;
        let retrieval_config = RetrievalConfig {
            dense_top_k: 2,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let rerank_config = RerankConfig {
            enabled: false,
            top_n: 1,
            ..Default::default()
        };
        let reranker = RecordingReranker::hits(vec![(1, 1.0)]);

        let (results, debug) = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
        )
        .with_reranker(&rerank_config, &reranker)
        .search_filtered_with_debug("alpha", None)
        .await
        .unwrap();

        assert_eq!(chunk_ids(&results), vec!["chunk-first", "chunk-second"]);
        assert_eq!(reranker.call_count(), 0);
        assert_eq!(debug.reranker.status, RetrievalRerankStatus::Disabled);
        assert!(debug.reranker.scores.is_empty());
    }

    #[tokio::test]
    async fn rerank_failure_falls_back_to_rrf_order_with_bounded_debug() {
        let store = Store::in_memory().unwrap();
        let source = source("src-rerank-fallback");
        store.add_source(&source).unwrap();
        let first = insert_text_chunk(&store, &source, "chunk-first", "alpha first");
        let second = insert_text_chunk(&store, &source, "chunk-second", "alpha second");
        let vector_index =
            StaticVectorIndex::new(vec![(first.id.clone(), 0.9), (second.id.clone(), 0.8)]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let embed_client = KeywordEmbeddingClient;
        let retrieval_config = RetrievalConfig {
            dense_top_k: 2,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let rerank_config = RerankConfig {
            enabled: true,
            strategy: RerankStrategy::Endpoint,
            provider: "jina".into(),
            model: "fallback-reranker".into(),
            top_n: 2,
            ..Default::default()
        };
        let reranker = RecordingReranker::error("request timed out after a long provider message");

        let (results, debug) = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
        )
        .with_reranker(&rerank_config, &reranker)
        .search_filtered_with_debug("alpha", None)
        .await
        .unwrap();

        assert_eq!(chunk_ids(&results), vec!["chunk-first", "chunk-second"]);
        assert_eq!(debug.reranker.status, RetrievalRerankStatus::Fallback);
        assert_eq!(debug.reranker.reason.as_deref(), Some("request_timeout"));
        assert_eq!(debug.reranker.provider.as_deref(), Some("jina"));
        assert!(debug.reranker.scores.is_empty());
    }

    #[tokio::test]
    async fn rerank_ignores_out_of_range_duplicate_and_nonfinite_indices() {
        let store = Store::in_memory().unwrap();
        let source = source("src-rerank-invalid");
        store.add_source(&source).unwrap();
        let first = insert_text_chunk(&store, &source, "chunk-first", "alpha first");
        let second = insert_text_chunk(&store, &source, "chunk-second", "alpha second");
        let third = insert_text_chunk(&store, &source, "chunk-third", "alpha third");
        let vector_index = StaticVectorIndex::new(vec![
            (first.id.clone(), 0.9),
            (second.id.clone(), 0.8),
            (third.id.clone(), 0.7),
        ]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let embed_client = KeywordEmbeddingClient;
        let retrieval_config = RetrievalConfig {
            dense_top_k: 3,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let rerank_config = RerankConfig {
            enabled: true,
            top_n: 3,
            ..Default::default()
        };
        let reranker = RecordingReranker::hits(vec![
            (99, 0.99),
            (2, f32::NAN),
            (1, 0.8),
            (1, 0.7),
            (0, 0.8),
        ]);

        let (results, debug) = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
        )
        .with_reranker(&rerank_config, &reranker)
        .search_filtered_with_debug("alpha", None)
        .await
        .unwrap();

        assert_eq!(chunk_ids(&results), vec!["chunk-first", "chunk-second"]);
        assert_eq!(debug.reranker.status, RetrievalRerankStatus::Succeeded);
        assert_eq!(debug.reranker.scores.len(), 2);
    }

    #[tokio::test]
    async fn rerank_ignores_indices_outside_submitted_candidate_count() {
        let store = Store::in_memory().unwrap();
        let source = source("src-rerank-shaped");
        store.add_source(&source).unwrap();
        let first = insert_text_chunk(&store, &source, "chunk-first", "alpha first");
        let second = insert_text_chunk(&store, &source, "chunk-second", "alpha second");
        let third = insert_text_chunk(&store, &source, "chunk-third", "alpha third");
        let vector_index = StaticVectorIndex::new(vec![
            (first.id.clone(), 0.9),
            (second.id.clone(), 0.8),
            (third.id.clone(), 0.7),
        ]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let embed_client = KeywordEmbeddingClient;
        let retrieval_config = RetrievalConfig {
            dense_top_k: 3,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let rerank_config = RerankConfig {
            enabled: true,
            top_n: 3,
            ..Default::default()
        };
        let reranker = RecordingReranker::hits_with_request(
            vec![(2, 0.99), (0, 0.8)],
            RerankRequestDiagnostics {
                candidate_count: 1,
                document_char_limit: 512,
                top_n: 1,
            },
        );

        let (results, debug) = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
        )
        .with_reranker(&rerank_config, &reranker)
        .search_filtered_with_debug("alpha", None)
        .await
        .unwrap();

        assert_eq!(chunk_ids(&results), vec!["chunk-first"]);
        assert_eq!(debug.reranker.status, RetrievalRerankStatus::Succeeded);
        assert_eq!(debug.reranker.candidate_count, Some(1));
        assert_eq!(debug.reranker.scores.len(), 1);
        assert_eq!(debug.reranker.scores[0].chunk_id, first.id);
        assert_ne!(debug.reranker.scores[0].chunk_id, third.id);
    }

    #[tokio::test]
    async fn rerank_top_n_is_bounded_by_candidate_count() {
        let store = Store::in_memory().unwrap();
        let source = source("src-rerank-topn");
        store.add_source(&source).unwrap();
        let first = insert_text_chunk(&store, &source, "chunk-first", "alpha first");
        let second = insert_text_chunk(&store, &source, "chunk-second", "alpha second");
        let vector_index =
            StaticVectorIndex::new(vec![(first.id.clone(), 0.9), (second.id.clone(), 0.8)]);
        let lexical_index = StaticLexicalIndex::new(Vec::new());
        let embed_client = KeywordEmbeddingClient;
        let retrieval_config = RetrievalConfig {
            dense_top_k: 2,
            bm25_top_k: 0,
            rrf_k: 60,
            ..RetrievalConfig::default()
        };
        let rerank_config = RerankConfig {
            enabled: true,
            top_n: 999,
            ..Default::default()
        };
        let reranker = RecordingReranker::hits(vec![(1, 0.9), (0, 0.8)]);

        let (_results, debug) = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
        )
        .with_reranker(&rerank_config, &reranker)
        .search_filtered_with_debug("alpha", None)
        .await
        .unwrap();

        assert_eq!(reranker.recorded_top_ns(), vec![2]);
        assert_eq!(debug.reranker.top_n, Some(2));
        assert_eq!(debug.reranker.candidate_count, Some(2));
    }

    #[tokio::test]
    async fn graph_expansion_disabled_preserves_seed_results() {
        let store = Store::in_memory().unwrap();
        let source = source("src-disabled");
        store.add_source(&source).unwrap();
        let seed = insert_text_chunk(&store, &source, "chunk-seed", "alpha seed");
        let neighbor = insert_text_chunk(&store, &source, "chunk-neighbor", "neighbor context");
        upsert_chunk_graph_nodes(&store, &source.id, &[&seed, &neighbor]);

        let seed_node = GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &seed.id.0);
        let neighbor_node = GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &neighbor.id.0);
        store
            .upsert_graph_edges(&[graph_edge(
                &source.id,
                EdgeType::Next,
                &seed_node,
                &neighbor_node,
                0,
            )])
            .unwrap();

        let hnsw = hnsw_with_seed(&store, &seed);
        let lexical_index = SqliteFtsIndex::new(&store);
        let embed_client = KeywordEmbeddingClient;
        let retrieval_config = RetrievalConfig::default();
        let baseline = RetrievalPipeline::new(
            &hnsw,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
        )
        .search("alpha")
        .await
        .unwrap();
        let disabled_graph = GraphConfig {
            enabled: false,
            ..graph_config(vec![EdgeType::Next])
        };
        let with_disabled_graph = RetrievalPipeline::new_with_graph(
            &hnsw,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
            &disabled_graph,
        )
        .search("alpha")
        .await
        .unwrap();

        assert_eq!(chunk_ids(&with_disabled_graph), chunk_ids(&baseline));
        assert_eq!(
            with_disabled_graph[0].provenance.origin,
            RetrievalOrigin::Seed
        );
    }

    #[tokio::test]
    async fn graph_expansion_recovers_previous_and_next_neighbors_from_inverse_edges() {
        let store = Store::in_memory().unwrap();
        let source = source("src-adjacent");
        store.add_source(&source).unwrap();
        let previous = insert_text_chunk(&store, &source, "chunk-previous", "previous context");
        let seed = insert_text_chunk(&store, &source, "chunk-seed", "alpha seed");
        let next = insert_text_chunk(&store, &source, "chunk-next", "next context");
        upsert_chunk_graph_nodes(&store, &source.id, &[&previous, &seed, &next]);

        let previous_node = GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &previous.id.0);
        let seed_node = GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &seed.id.0);
        let next_node = GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &next.id.0);
        store
            .upsert_graph_edges(&[
                graph_edge(&source.id, EdgeType::Next, &previous_node, &seed_node, 0),
                graph_edge(&source.id, EdgeType::Previous, &next_node, &seed_node, 1),
            ])
            .unwrap();

        let hnsw = hnsw_with_seed(&store, &seed);
        let lexical_index = SqliteFtsIndex::new(&store);
        let embed_client = KeywordEmbeddingClient;
        let retrieval_config = RetrievalConfig::default();
        let graph_config = graph_config(vec![EdgeType::Previous, EdgeType::Next]);
        let results = RetrievalPipeline::new_with_graph(
            &hnsw,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
            &graph_config,
        )
        .search("alpha")
        .await
        .unwrap();

        let ids = chunk_ids(&results);
        assert_eq!(ids[0], "chunk-seed");
        assert!(ids.contains(&"chunk-previous".to_string()));
        assert!(ids.contains(&"chunk-next".to_string()));

        let expanded = results
            .iter()
            .find(|result| result.chunk_id == previous.id)
            .unwrap();
        assert_eq!(expanded.provenance.origin, RetrievalOrigin::GraphExpansion);
        assert_eq!(expanded.provenance.seed_chunk_id, Some(seed.id.clone()));
        assert_eq!(expanded.provenance.seed_source_id, Some(source.id.clone()));
        assert_eq!(expanded.provenance.hop_distance, 1);
        assert_eq!(
            expanded.provenance.graph_path[0].direction,
            GraphTraversalDirection::Incoming
        );
        assert!(expanded.score < results[0].score);
    }

    #[tokio::test]
    async fn search_debug_reports_seed_rankings_graph_path_and_final_pack() {
        let store = Store::in_memory().unwrap();
        let source = source("src-debug");
        store.add_source(&source).unwrap();
        let seed = insert_text_chunk(&store, &source, "chunk-seed", "alpha seed");
        let neighbor = insert_text_chunk(&store, &source, "chunk-neighbor", "neighbor context");
        upsert_chunk_graph_nodes(&store, &source.id, &[&seed, &neighbor]);

        let seed_node = GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &seed.id.0);
        let neighbor_node = GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &neighbor.id.0);
        store
            .upsert_graph_edges(&[graph_edge(
                &source.id,
                EdgeType::Next,
                &seed_node,
                &neighbor_node,
                0,
            )])
            .unwrap();

        let hnsw = hnsw_with_seed(&store, &seed);
        let lexical_index = SqliteFtsIndex::new(&store);
        let embed_client = KeywordEmbeddingClient;
        let retrieval_config = RetrievalConfig::default();
        let graph_config = graph_config(vec![EdgeType::Next]);
        let (_results, debug) = RetrievalPipeline::new_with_graph(
            &hnsw,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
            &graph_config,
        )
        .search_filtered_with_debug("alpha", None)
        .await
        .unwrap();

        assert!(!debug.bm25_hits.is_empty());
        assert!(!debug.dense_hits.is_empty());
        assert!(!debug.rrf_fused_hits.is_empty());
        assert_eq!(debug.rrf_fused_hits[0].rank, 1);
        assert_eq!(debug.rrf_fused_hits[0].chunk_id, seed.id);
        assert_eq!(debug.rrf_fused_hits[0].dense_rank, Some(1));

        assert_eq!(debug.graph_expanded_hits.len(), 1);
        let expanded = &debug.graph_expanded_hits[0];
        assert_eq!(expanded.seed_rank, 1);
        assert_eq!(expanded.seed_chunk_id, seed.id);
        assert_eq!(expanded.expanded_chunk_id, neighbor.id);
        assert_eq!(expanded.hop_distance, 1);
        assert_eq!(expanded.path[0].edge_type, EdgeType::Next);
        assert_eq!(
            expanded.path[0].direction,
            GraphTraversalDirection::Outgoing
        );

        assert_eq!(debug.reranker.status, RetrievalRerankStatus::Disabled);
        assert_eq!(debug.reranker.scores, Vec::new());
        assert!(debug
            .final_evidence_pack
            .iter()
            .any(
                |entry| entry.evidence_id == EvidenceId("ev-chunk-seed".into())
                    && entry.role == RetrievalEvidenceRole::OriginalText
            ));

        let encoded = serde_json::to_string(&debug).unwrap();
        assert!(!encoded.contains("alpha seed"));
        assert!(!encoded.contains("neighbor context"));
    }

    #[test]
    fn debug_search_output_returns_error_when_debug_is_missing() {
        let output = RetrievalSearchOutput {
            results: Vec::new(),
            debug: None,
        };

        let error = output.into_results_with_debug().unwrap_err();

        assert!(error
            .to_string()
            .contains("retrieval debug output missing after debug search"));
    }

    #[tokio::test]
    async fn graph_expansion_recovers_parent_chunk_evidence() {
        let store = Store::in_memory().unwrap();
        let source = source("src-parent");
        store.add_source(&source).unwrap();
        let parent_extra = EvidenceUnit {
            id: EvidenceId("ev-parent-extra".into()),
            source_id: source.id.clone(),
            kind: EvidenceKind::Text,
            derived_from: None,
            locator: SourceLocator::Document {
                path_or_url: source.path.to_string_lossy().into_owned(),
                line_start: 2,
                line_end: None,
            },
            text: "Parent-only context.".into(),
            text_hash: "hash-parent-extra".into(),
            heading_path: Vec::new(),
            position: 1,
        };
        store
            .bulk_insert_evidence(std::slice::from_ref(&parent_extra))
            .unwrap();
        let parent = insert_parent_chunk(
            &store,
            &source,
            "chunk-parent",
            "Parent-only context.",
            vec![parent_extra.id.clone()],
        );
        let seed = insert_text_chunk_with_parent(
            &store,
            &source,
            "chunk-child",
            "alpha child",
            parent.id.clone(),
        );
        store
            .link_chunk_evidence(&[(parent.id.clone(), seed.evidence_unit_ids[0].clone())])
            .unwrap();
        upsert_chunk_graph_nodes(&store, &source.id, &[&seed, &parent]);

        let seed_node = GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &seed.id.0);
        let parent_node = GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &parent.id.0);
        store
            .upsert_graph_edges(&[graph_edge(
                &source.id,
                EdgeType::Parent,
                &seed_node,
                &parent_node,
                0,
            )])
            .unwrap();

        let hnsw = hnsw_with_seed(&store, &seed);
        let lexical_index = SqliteFtsIndex::new(&store);
        let embed_client = KeywordEmbeddingClient;
        let retrieval_config = RetrievalConfig::default();
        let graph_config = graph_config(vec![EdgeType::Parent]);
        let results = RetrievalPipeline::new_with_graph(
            &hnsw,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
            &graph_config,
        )
        .search("alpha")
        .await
        .unwrap();

        let parent_result = results
            .iter()
            .find(|result| result.chunk_id == parent.id)
            .unwrap();
        assert!(parent_result
            .evidence_units
            .iter()
            .any(|evidence| evidence.id == parent_extra.id));
        assert_eq!(
            parent_result.provenance.graph_path[0].edge_type,
            EdgeType::Parent
        );
    }

    #[tokio::test]
    async fn graph_expansion_recovers_section_context_without_whole_source() {
        let store = Store::in_memory().unwrap();
        let source = source("src-section");
        store.add_source(&source).unwrap();
        let heading = vec!["Intro".to_string()];
        let seed = insert_text_chunk_with_heading(
            &store,
            &source,
            "chunk-seed",
            "alpha seed",
            heading.clone(),
        );
        let sibling = insert_text_chunk_with_heading(
            &store,
            &source,
            "chunk-sibling",
            "sibling context",
            heading,
        );
        let outside = insert_text_chunk_with_heading(
            &store,
            &source,
            "chunk-outside",
            "outside context",
            vec!["Other".to_string()],
        );
        let section = graph_node(&source.id, GraphNodeKind::Section, "section:Intro");
        store
            .upsert_graph_nodes(std::slice::from_ref(&section))
            .unwrap();
        upsert_chunk_graph_nodes(&store, &source.id, &[&seed, &sibling, &outside]);
        let seed_node = GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &seed.id.0);
        let sibling_node = GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &sibling.id.0);
        store
            .upsert_graph_edges(&[
                graph_edge(
                    &source.id,
                    EdgeType::SectionContains,
                    &section.id,
                    &seed_node,
                    0,
                ),
                graph_edge(
                    &source.id,
                    EdgeType::SectionContains,
                    &section.id,
                    &sibling_node,
                    1,
                ),
            ])
            .unwrap();

        let hnsw = hnsw_with_seed(&store, &seed);
        let lexical_index = SqliteFtsIndex::new(&store);
        let embed_client = KeywordEmbeddingClient;
        let retrieval_config = RetrievalConfig::default();
        let graph_config = graph_config(vec![EdgeType::SectionContains]);
        let results = RetrievalPipeline::new_with_graph(
            &hnsw,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
            &graph_config,
        )
        .search("alpha")
        .await
        .unwrap();

        let ids = chunk_ids(&results);
        assert!(ids.contains(&sibling.id.0));
        assert!(!ids.contains(&outside.id.0));
        let sibling_result = results
            .iter()
            .find(|result| result.chunk_id == sibling.id)
            .unwrap();
        assert_eq!(sibling_result.provenance.hop_distance, 1);
        assert_eq!(sibling_result.provenance.graph_path.len(), 2);
    }

    #[tokio::test]
    async fn graph_expansion_recovers_page_image_candidates() {
        let store = Store::in_memory().unwrap();
        let source = source("src-image");
        store.add_source(&source).unwrap();
        let seed = insert_pdf_text_chunk(&store, &source, "chunk-seed", "alpha page text", 7);
        let (image_chunk, artifact) =
            insert_image_chunk(&store, &source, "chunk-image", "img-1", 7);
        let page_node = graph_node(&source.id, GraphNodeKind::Page, "page:7");
        let image_node = graph_node(
            &source.id,
            GraphNodeKind::ImageArtifact,
            &artifact.image_id.0,
        );
        store
            .upsert_graph_nodes(&[page_node.clone(), image_node.clone()])
            .unwrap();
        upsert_chunk_graph_nodes(&store, &source.id, &[&seed, &image_chunk]);
        store
            .upsert_graph_edges(&[graph_edge(
                &source.id,
                EdgeType::PageContainsImage,
                &page_node.id,
                &image_node.id,
                0,
            )])
            .unwrap();

        let hnsw = hnsw_with_seed(&store, &seed);
        let lexical_index = SqliteFtsIndex::new(&store);
        let embed_client = KeywordEmbeddingClient;
        let retrieval_config = RetrievalConfig::default();
        let graph_config = graph_config(vec![EdgeType::PageContainsImage]);
        let results = RetrievalPipeline::new_with_graph(
            &hnsw,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
            &graph_config,
        )
        .search("alpha")
        .await
        .unwrap();

        let image_result = results
            .iter()
            .find(|result| result.chunk_id == image_chunk.id)
            .unwrap();
        assert!(image_result
            .evidence_units
            .iter()
            .any(|evidence| evidence.kind == EvidenceKind::Image));
        assert_eq!(
            image_result.provenance.graph_path[0].edge_type,
            EdgeType::PageContainsImage
        );
    }

    #[tokio::test]
    async fn graph_expansion_bounds_page_image_candidate_edges_before_resolution() {
        let store = Store::in_memory().unwrap();
        let source = source("src-image-budget");
        store.add_source(&source).unwrap();
        let seed = insert_pdf_text_chunk(&store, &source, "chunk-seed", "alpha page text", 7);
        let (image_chunk, artifact) =
            insert_image_chunk(&store, &source, "chunk-image", "img-valid", 7);
        let page_node = graph_node(&source.id, GraphNodeKind::Page, "page:7");
        let missing_image_nodes = (0..5)
            .map(|idx| {
                graph_node(
                    &source.id,
                    GraphNodeKind::ImageArtifact,
                    &format!("img-missing-{idx}"),
                )
            })
            .collect::<Vec<_>>();
        let valid_image_node = graph_node(
            &source.id,
            GraphNodeKind::ImageArtifact,
            &artifact.image_id.0,
        );
        let mut graph_nodes = vec![page_node.clone(), valid_image_node.clone()];
        graph_nodes.extend(missing_image_nodes.iter().cloned());
        store.upsert_graph_nodes(&graph_nodes).unwrap();
        upsert_chunk_graph_nodes(&store, &source.id, &[&seed, &image_chunk]);

        let mut edges = missing_image_nodes
            .iter()
            .enumerate()
            .map(|(ordinal, missing_node)| {
                graph_edge(
                    &source.id,
                    EdgeType::PageContainsImage,
                    &page_node.id,
                    &missing_node.id,
                    ordinal as u32,
                )
            })
            .collect::<Vec<_>>();
        edges.push(graph_edge(
            &source.id,
            EdgeType::PageContainsImage,
            &page_node.id,
            &valid_image_node.id,
            99,
        ));
        store.upsert_graph_edges(&edges).unwrap();

        let hnsw = hnsw_with_seed(&store, &seed);
        let lexical_index = SqliteFtsIndex::new(&store);
        let embed_client = KeywordEmbeddingClient;
        let retrieval_config = RetrievalConfig::default();
        let graph_config = GraphConfig {
            max_expanded_chunks: 3,
            max_neighbors_per_seed: 3,
            ..graph_config(vec![EdgeType::PageContainsImage])
        };
        let results = RetrievalPipeline::new_with_graph(
            &hnsw,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
            &graph_config,
        )
        .search("alpha")
        .await
        .unwrap();

        assert_eq!(chunk_ids(&results), vec!["chunk-seed".to_string()]);
    }

    #[tokio::test]
    async fn graph_expansion_respects_edge_type_filter() {
        let store = Store::in_memory().unwrap();
        let source = source("src-filter");
        store.add_source(&source).unwrap();
        let seed = insert_text_chunk(&store, &source, "chunk-seed", "alpha seed");
        let neighbor = insert_text_chunk(&store, &source, "chunk-neighbor", "neighbor context");
        upsert_chunk_graph_nodes(&store, &source.id, &[&seed, &neighbor]);
        let seed_node = GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &seed.id.0);
        let neighbor_node = GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &neighbor.id.0);
        store
            .upsert_graph_edges(&[graph_edge(
                &source.id,
                EdgeType::Next,
                &seed_node,
                &neighbor_node,
                0,
            )])
            .unwrap();

        let hnsw = hnsw_with_seed(&store, &seed);
        let lexical_index = SqliteFtsIndex::new(&store);
        let embed_client = KeywordEmbeddingClient;
        let retrieval_config = RetrievalConfig::default();
        let graph_config = graph_config(vec![EdgeType::Parent]);
        let results = RetrievalPipeline::new_with_graph(
            &hnsw,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
            &graph_config,
        )
        .search("alpha")
        .await
        .unwrap();

        assert_eq!(chunk_ids(&results), vec!["chunk-seed".to_string()]);
    }

    #[tokio::test]
    async fn graph_expansion_respects_neighbor_and_global_budgets() {
        let store = Store::in_memory().unwrap();
        let source = source("src-budget");
        store.add_source(&source).unwrap();
        let seed = insert_text_chunk(&store, &source, "chunk-seed", "alpha seed");
        let targets = ["chunk-a", "chunk-b", "chunk-c"]
            .into_iter()
            .map(|id| insert_text_chunk(&store, &source, id, "linked context"))
            .collect::<Vec<_>>();
        let mut chunk_refs = vec![&seed];
        chunk_refs.extend(targets.iter());
        upsert_chunk_graph_nodes(&store, &source.id, &chunk_refs);

        let seed_node = GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &seed.id.0);
        let edges = targets
            .iter()
            .enumerate()
            .map(|(ordinal, chunk)| {
                let target_node = GraphNodeId::new(&source.id, GraphNodeKind::Chunk, &chunk.id.0);
                graph_edge(
                    &source.id,
                    EdgeType::MarkdownLinksTo,
                    &seed_node,
                    &target_node,
                    ordinal as u32,
                )
            })
            .collect::<Vec<_>>();
        store.upsert_graph_edges(&edges).unwrap();

        let hnsw = hnsw_with_seed(&store, &seed);
        let lexical_index = SqliteFtsIndex::new(&store);
        let embed_client = KeywordEmbeddingClient;
        let retrieval_config = RetrievalConfig::default();
        let neighbor_limited = GraphConfig {
            max_neighbors_per_seed: 2,
            ..graph_config(vec![EdgeType::MarkdownLinksTo])
        };
        let neighbor_limited_results = RetrievalPipeline::new_with_graph(
            &hnsw,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
            &neighbor_limited,
        )
        .search("alpha")
        .await
        .unwrap();
        assert_eq!(neighbor_limited_results.len(), 3);

        let globally_limited = GraphConfig {
            max_expanded_chunks: 1,
            ..graph_config(vec![EdgeType::MarkdownLinksTo])
        };
        let globally_limited_results = RetrievalPipeline::new_with_graph(
            &hnsw,
            &lexical_index,
            &store,
            &embed_client,
            &retrieval_config,
            &globally_limited,
        )
        .search("alpha")
        .await
        .unwrap();
        assert_eq!(globally_limited_results.len(), 2);
    }

    fn chunk_ids(results: &[RetrievalResult]) -> Vec<String> {
        results
            .iter()
            .map(|result| result.chunk_id.0.clone())
            .collect()
    }
}
