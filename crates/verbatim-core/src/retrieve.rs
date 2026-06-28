use std::collections::{HashMap, HashSet};
use std::time::Instant;

use anyhow::{anyhow, Result};

use crate::config::{GraphConfig, QdrantConfig, RerankConfig, RetrievalConfig};
#[cfg(feature = "qdrant")]
use crate::index::qdrant::{QdrantClient, QdrantHit};
use crate::provider::ProviderError;
use crate::store::Store;
use crate::traits::{
    EmbeddingClient, LexicalIndex, RerankCapabilityState, RerankDiagnostics, RerankError,
    RerankRequestDiagnostics, Reranker, VectorIndex,
};
use crate::types::{
    Chunk, ChunkId, ChunkType, EdgeType, EmbeddingProfileId, EvidenceId, EvidenceKind,
    EvidenceUnit, GraphExpansionStep, GraphNodeId, GraphNodeKind, GraphTraversalDirection, ImageId,
    RetrievalDebug, RetrievalDenseVectorPath, RetrievalEvidencePackEntry, RetrievalEvidenceRole,
    RetrievalFusedHit, RetrievalGraphExpansionDebug, RetrievalLocatorDebug, RetrievalProvenance,
    RetrievalRerankCapabilityDebug, RetrievalRerankCapabilityState, RetrievalRerankDebug,
    RetrievalRerankRequestDebug, RetrievalRerankScore, RetrievalResult, RetrievalStageHit,
    SourceId, SourceLocator, VectorIndexResidency,
};

const GRAPH_EXPANSION_SCORE_DECAY: f32 = 0.5;
const MAX_RERANK_CANDIDATE_CHUNKS: usize = 50;
const MAX_RERANK_DOCUMENT_CHARS: usize = 8_000;

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
    required_profile_id: Option<EmbeddingProfileId>,
    vector_residency: VectorIndexResidency,
    #[cfg(feature = "qdrant")]
    qdrant: Option<QdrantClient>,
}

#[cfg(feature = "qdrant")]
trait DenseHit {
    fn chunk_id(&self) -> ChunkId;
    fn score(&self) -> f32;
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
            required_profile_id: None,
            vector_residency: VectorIndexResidency::ResidentHnsw,
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
            required_profile_id: None,
            vector_residency: VectorIndexResidency::ResidentHnsw,
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

    pub fn with_reranker(mut self, config: &'a RerankConfig, reranker: &'a dyn Reranker) -> Self {
        self.rerank_config = Some(config);
        self.reranker = Some(reranker);
        self
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
            .search_filtered_internal(query, source_filter, false)
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
        self.search_filtered_internal(query, source_filter, true)
            .await?
            .into_results_with_debug()
    }

    async fn search_filtered_internal(
        &self,
        query: &str,
        source_filter: Option<&HashSet<SourceId>>,
        include_debug: bool,
    ) -> Result<RetrievalSearchOutput> {
        if source_filter.is_some_and(HashSet::is_empty) {
            return Ok(empty_search_output(include_debug));
        }
        if self.embedding_enabled {
            self.ensure_required_profile_vectors(source_filter)?;
        }

        let all_child_count = if source_filter.is_some() {
            self.store.list_child_chunks()?.len()
        } else {
            0
        };
        #[cfg(feature = "qdrant")]
        let qdrant_can_filter = self.qdrant.is_some();
        #[cfg(not(feature = "qdrant"))]
        let qdrant_can_filter = false;
        let dense_top_k = if !self.embedding_enabled {
            0
        } else if source_filter.is_some()
            && !qdrant_can_filter
            && !self.vector_index.supports_source_filter()
        {
            self.vector_index.len().max(self.config.dense_top_k)
        } else {
            self.config.dense_top_k
        };
        let bm25_top_k = source_filter
            .map(|_| all_child_count.max(self.config.bm25_top_k))
            .unwrap_or(self.config.bm25_top_k);

        let (dense_results, query_embedding_latency_ms, dense_vector_path) =
            if self.embedding_enabled {
                let query_text = self.embed_client.prepare_query(query);
                let embedding_started = Instant::now();
                let query_vec = self
                    .embed_client
                    .embed(&[query_text])
                    .await?
                    .into_iter()
                    .next()
                    .unwrap_or_default();
                let query_embedding_latency_ms = elapsed_ms(embedding_started);
                (
                    self.dense_search(&query_vec, dense_top_k, source_filter)
                        .await?,
                    Some(query_embedding_latency_ms),
                    self.dense_vector_path(),
                )
            } else {
                (Vec::new(), None, RetrievalDenseVectorPath::Bm25Only)
            };

        let bm25_results = self.lexical_index.search(query, bm25_top_k)?;

        let mut fused = rrf_fusion(&dense_results, &bm25_results, self.config.rrf_k);
        if let Some(source_ids) = source_filter {
            fused.retain(|(chunk_id, _)| {
                self.store
                    .get_chunk(chunk_id)
                    .ok()
                    .flatten()
                    .is_some_and(|chunk| source_ids.contains(&chunk.source_id))
            });
        }

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

        let RerankOutcome {
            fused,
            debug: reranker_debug,
        } = self.rerank_fused(query, fused).await?;

        let mut results = Vec::new();
        for (rank, (chunk_id, score)) in fused.into_iter().enumerate() {
            let Some(chunk) = self.store.get_chunk(&chunk_id)? else {
                continue;
            };
            let result_rank = rank + 1;
            let provenance =
                RetrievalProvenance::seed(result_rank, chunk.id.clone(), chunk.source_id.clone());

            results.push(self.result_for_chunk(chunk, score, provenance)?);
        }

        self.expand_graph_results(&mut results, source_filter)?;

        let debug = if include_debug {
            Some(RetrievalDebug {
                dense_vector_path,
                query_embedding_latency_ms,
                bm25_hits,
                dense_hits,
                rrf_fused_hits,
                graph_expanded_hits: graph_expansion_debug_hits(&results),
                reranker: reranker_debug,
                final_evidence_pack: final_evidence_pack_debug(&results),
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

    async fn dense_search(
        &self,
        query_vec: &[f32],
        top_k: usize,
        source_filter: Option<&HashSet<SourceId>>,
    ) -> Result<Vec<(ChunkId, f32)>> {
        #[cfg(feature = "qdrant")]
        if let Some(qdrant) = &self.qdrant {
            let local_results = self.local_dense_search(query_vec, top_k, source_filter)?;
            if local_results.is_empty() {
                return Ok(local_results);
            }
            let default_profile_id;
            let profile_id = match &self.required_profile_id {
                Some(profile_id) => profile_id,
                None => {
                    default_profile_id = EmbeddingProfileId::default_profile();
                    &default_profile_id
                }
            };
            let qdrant_source_filter = single_source_filter(source_filter);
            let profile_generation = self.store.index_generation_for_profile(profile_id)?;
            return match qdrant
                .search(profile_id, query_vec, top_k, qdrant_source_filter)
                .await
            {
                Ok(results) => self.merge_preferred_dense_hits(
                    profile_id,
                    profile_generation,
                    results,
                    local_results,
                    top_k,
                    source_filter,
                ),
                Err(err) => {
                    tracing::warn!(
                        error = %err,
                        "qdrant search failed; falling back to local dense index"
                    );
                    self.valid_dense_hits(local_results, top_k, source_filter)
                }
            };
        }
        self.local_dense_search(query_vec, top_k, source_filter)
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
        let fallback_top_k = if source_filter.is_some() {
            self.vector_index.len().max(top_k)
        } else {
            top_k
        };
        let index_source_filter = single_source_filter(source_filter);
        Ok(self
            .vector_index
            .search_filtered(query_vec, fallback_top_k, index_source_filter))
    }

    fn dense_vector_path(&self) -> RetrievalDenseVectorPath {
        match self.vector_residency {
            VectorIndexResidency::LowMemory => RetrievalDenseVectorPath::LowMemorySqliteScan,
            VectorIndexResidency::ResidentHnsw => RetrievalDenseVectorPath::ResidentHnsw,
        }
    }

    #[cfg(feature = "qdrant")]
    fn valid_dense_hits(
        &self,
        hits: Vec<(ChunkId, f32)>,
        top_k: usize,
        source_filter: Option<&HashSet<SourceId>>,
    ) -> Result<Vec<(ChunkId, f32)>> {
        let mut valid = Vec::new();
        let mut seen = HashSet::new();
        let mut hits = hits.into_iter();
        while valid.len() < top_k
            && self.append_next_valid_dense_hit(
                &mut valid,
                &mut seen,
                &mut hits,
                source_filter,
                None,
            )?
        {}
        Ok(valid)
    }

    #[cfg(feature = "qdrant")]
    fn merge_preferred_dense_hits(
        &self,
        profile_id: &EmbeddingProfileId,
        profile_generation: u64,
        preferred_hits: Vec<QdrantHit>,
        fallback_hits: Vec<(ChunkId, f32)>,
        top_k: usize,
        source_filter: Option<&HashSet<SourceId>>,
    ) -> Result<Vec<(ChunkId, f32)>> {
        let mut merged = Vec::new();
        let mut seen = HashSet::new();
        let mut preferred_hits = preferred_hits.into_iter();
        let mut fallback_hits = fallback_hits.into_iter();

        while merged.len() < top_k {
            let preferred_added = self.append_next_valid_dense_hit(
                &mut merged,
                &mut seen,
                &mut preferred_hits,
                source_filter,
                Some((profile_id, profile_generation)),
            )?;
            if merged.len() >= top_k {
                break;
            }
            let fallback_added = self.append_next_valid_dense_hit(
                &mut merged,
                &mut seen,
                &mut fallback_hits,
                source_filter,
                None,
            )?;
            if !preferred_added && !fallback_added {
                break;
            }
        }

        Ok(merged)
    }

    #[cfg(feature = "qdrant")]
    fn append_next_valid_dense_hit<I>(
        &self,
        target: &mut Vec<(ChunkId, f32)>,
        seen: &mut HashSet<ChunkId>,
        hits: &mut I,
        source_filter: Option<&HashSet<SourceId>>,
        required_profile: Option<(&EmbeddingProfileId, u64)>,
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
            if source_filter_excludes(source_filter, &chunk.source_id) {
                continue;
            }
            if let Some((profile_id, profile_generation)) = required_profile {
                if hit.profile_generation() != Some(profile_generation) {
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
        match reranker
            .rerank_with_diagnostics(query, &documents, top_n)
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
        if source_filter_excludes(state.source_filter, &result.chunk.source_id) {
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
            bm25_hits: Vec::new(),
            dense_hits: Vec::new(),
            rrf_fused_hits: Vec::new(),
            graph_expanded_hits: Vec::new(),
            reranker: RetrievalRerankDebug::disabled(),
            final_evidence_pack: Vec::new(),
        }),
    }
}

fn single_source_filter(source_filter: Option<&HashSet<SourceId>>) -> Option<&SourceId> {
    let source_ids = source_filter?;
    if source_ids.len() == 1 {
        source_ids.iter().next()
    } else {
        None
    }
}

fn source_filter_excludes(source_filter: Option<&HashSet<SourceId>>, source_id: &SourceId) -> bool {
    source_filter.is_some_and(|source_ids| !source_ids.contains(source_id))
}

fn source_filter_scope(source_filter: Option<&HashSet<SourceId>>) -> String {
    match source_filter {
        Some(source_ids) if source_ids.len() == 1 => source_ids
            .iter()
            .next()
            .map(|source_id| format!(" for source '{}'", source_id.0))
            .unwrap_or_default(),
        Some(source_ids) => format!(" for {} selected sources", source_ids.len()),
        None => String::new(),
    }
}

fn source_filter_ingest_hint(source_filter: Option<&HashSet<SourceId>>) -> String {
    match source_filter {
        Some(source_ids) if source_ids.len() == 1 => source_ids
            .iter()
            .next()
            .map(|source_id| format!(" {}", source_id.0))
            .unwrap_or_default(),
        _ => String::new(),
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
    debug.final_evidence_pack = final_evidence_pack_debug(results);
}

fn final_evidence_pack_debug(results: &[RetrievalResult]) -> Vec<RetrievalEvidencePackEntry> {
    let mut entries = Vec::new();
    let mut seen = HashSet::new();

    for result in results {
        for evidence in &result.evidence_units {
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
        SourceLocator::Document { .. } | SourceLocator::Markdown { .. } => None,
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

    use crate::index::hnsw::HnswIndex;
    use crate::index::sqlite_fts::SqliteFtsIndex;
    use crate::store::Store;
    use crate::traits::{LexicalIndex, VectorDocument, VectorIndex};
    use crate::types::{
        BBox, EdgeType, EvidenceId, EvidenceKind, GraphEdge, GraphEdgeId, GraphNode, GraphNodeId,
        GraphNodeKind, GraphTraversalDirection, ImageArtifact, ImageId, RetrievalDenseVectorPath,
        RetrievalEvidenceRole, RetrievalOrigin, RetrievalRerankStatus, Source, SourceLocator,
        SourceStatus, VectorIndexResidency,
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
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk).expect("read qdrant request");
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..n]);
            if String::from_utf8_lossy(&buffer).contains("\r\n\r\n") {
                break;
            }
        }
        String::from_utf8(buffer)
            .expect("request utf8")
            .lines()
            .next()
            .unwrap_or_default()
            .to_string()
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

    #[tokio::test]
    async fn retrieval_source_filter_applies_after_lexical_and_dense_search() {
        let store = Store::in_memory().unwrap();
        let first = source("src-1");
        let second = source("src-2");
        let alpha = insert_child(&store, &first, "chunk-alpha", "alpha content");
        let beta = insert_child(&store, &second, "chunk-beta", "beta content");
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
            ])
            .unwrap();
        let mut hnsw = HnswIndex::new();
        hnsw.rebuild_from_store(&store).unwrap();
        let lexical_index = SqliteFtsIndex::new(&store);
        let embed_client = KeywordEmbeddingClient;
        let config = RetrievalConfig::default();
        let pipeline =
            RetrievalPipeline::new(&hnsw, &lexical_index, &store, &embed_client, &config);

        let results = pipeline
            .search_filtered("beta", Some(&second.id))
            .await
            .unwrap();

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_id.0, "chunk-beta");
        assert_eq!(results[0].chunk.source_id, second.id);
        assert_eq!(results[0].evidence_units.len(), 1);
    }

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
        assert!(debug.dense_hits.is_empty());
        assert!(!debug.bm25_hits.is_empty());
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
        assert!(handle.join().unwrap().is_none());
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
    async fn qdrant_empty_success_fills_from_local_dense_index_with_source_filter() {
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

        assert_eq!(results.len(), 1);
        assert_eq!(results[0].chunk_id, wanted_chunk.id);
        assert_eq!(results[0].chunk.source_id, wanted_source.id);
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
            r#"{"status":"ok","result":[{"score":0.99,"payload":{"chunk_id":"chunk-remote-preferred","profile_generation":1}}]}"#,
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

        let results = pipeline
            .search_filtered("alpha", Some(&source.id))
            .await
            .unwrap();

        assert_eq!(results.len(), 2);
        assert_eq!(results[0].chunk_id, remote_preferred.id);
        assert_eq!(results[1].chunk_id, local_fallback.id);
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
            r#"{"status":"ok","result":[{"score":0.99,"payload":{"chunk_id":"src-qdrant-stale-existing-child-0","profile_generation":0}},{"score":0.98,"payload":{"chunk_id":"src-qdrant-stale-existing-child-2","profile_generation":0}}]}"#,
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
            r#"{"status":"ok","result":[{"score":0.99,"payload":{"chunk_id":"chunk-rebuilt-same-id","profile_generation":1}}]}"#,
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
