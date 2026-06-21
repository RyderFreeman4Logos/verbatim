use std::collections::HashMap;

use anyhow::Result;

use crate::config::RetrievalConfig;
use crate::embed::OpenAiEmbeddingClient;
use crate::index::hnsw::HnswIndex;
use crate::index::tantivy_bm25::Bm25Index;
use crate::store::Store;
use crate::traits::EmbeddingClient;
use crate::types::{ChunkId, ChunkType, EvidenceUnit, RetrievalResult, SourceId};

pub struct RetrievalPipeline<'a> {
    hnsw: &'a HnswIndex,
    bm25: &'a Bm25Index,
    store: &'a Store,
    embed_client: &'a OpenAiEmbeddingClient,
    config: &'a RetrievalConfig,
}

impl<'a> RetrievalPipeline<'a> {
    pub fn new(
        hnsw: &'a HnswIndex,
        bm25: &'a Bm25Index,
        store: &'a Store,
        embed_client: &'a OpenAiEmbeddingClient,
        config: &'a RetrievalConfig,
    ) -> Self {
        Self {
            hnsw,
            bm25,
            store,
            embed_client,
            config,
        }
    }

    pub async fn search(&self, query: &str) -> Result<Vec<RetrievalResult>> {
        self.search_filtered(query, None).await
    }

    pub async fn search_filtered(
        &self,
        query: &str,
        source_filter: Option<&SourceId>,
    ) -> Result<Vec<RetrievalResult>> {
        let query_text = self.embed_client.prepare_query(query);
        let query_vec = self
            .embed_client
            .embed(&[query_text])
            .await?
            .into_iter()
            .next()
            .unwrap_or_default();

        let all_child_count = if source_filter.is_some() {
            self.store.list_child_chunks()?.len()
        } else {
            0
        };
        let dense_top_k = source_filter
            .map(|_| self.hnsw.len().max(self.config.dense_top_k))
            .unwrap_or(self.config.dense_top_k);
        let bm25_top_k = source_filter
            .map(|_| all_child_count.max(self.config.bm25_top_k))
            .unwrap_or(self.config.bm25_top_k);

        let dense_results = self.hnsw.search(&query_vec, dense_top_k);

        let bm25_results = self.bm25.search(query, bm25_top_k)?;

        let mut fused = rrf_fusion(&dense_results, &bm25_results, self.config.rrf_k);
        if let Some(source_id) = source_filter {
            fused.retain(|(chunk_id, _)| {
                self.store
                    .get_chunk(chunk_id)
                    .ok()
                    .flatten()
                    .is_some_and(|chunk| chunk.source_id == *source_id)
            });
        }

        let mut results = Vec::new();
        for (chunk_id, score) in fused {
            let chunk = match self.store.get_chunk(&chunk_id)? {
                Some(c) => c,
                None => continue,
            };

            let parent_chunk = if chunk.chunk_type == ChunkType::Child {
                chunk
                    .parent_chunk_id
                    .as_ref()
                    .and_then(|pid| self.store.get_chunk(pid).ok().flatten())
            } else {
                None
            };

            let display_chunk = parent_chunk.unwrap_or_else(|| chunk.clone());

            let evidence_units: Vec<EvidenceUnit> = chunk
                .evidence_unit_ids
                .iter()
                .filter_map(|eid| self.store.get_evidence(eid).ok().flatten())
                .collect();

            results.push(RetrievalResult {
                chunk_id: chunk.id.clone(),
                score,
                chunk: display_chunk,
                evidence_units,
            });
        }

        Ok(results)
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

#[cfg(test)]
mod tests {
    use super::*;

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
}
