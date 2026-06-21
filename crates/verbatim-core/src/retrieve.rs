use std::collections::HashMap;

use anyhow::Result;

use crate::config::RetrievalConfig;
use crate::store::Store;
use crate::traits::{EmbeddingClient, LexicalIndex, VectorIndex};
use crate::types::{ChunkId, ChunkType, EvidenceUnit, RetrievalResult, SourceId};

pub struct RetrievalPipeline<'a> {
    vector_index: &'a dyn VectorIndex,
    lexical_index: &'a dyn LexicalIndex,
    store: &'a Store,
    embed_client: &'a dyn EmbeddingClient,
    config: &'a RetrievalConfig,
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
            .map(|_| self.vector_index.len().max(self.config.dense_top_k))
            .unwrap_or(self.config.dense_top_k);
        let bm25_top_k = source_filter
            .map(|_| all_child_count.max(self.config.bm25_top_k))
            .unwrap_or(self.config.bm25_top_k);

        let dense_results = self.vector_index.search(&query_vec, dense_top_k);

        let bm25_results = self.lexical_index.search(query, bm25_top_k)?;

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
    use async_trait::async_trait;

    use crate::index::hnsw::HnswIndex;
    use crate::index::sqlite_fts::SqliteFtsIndex;
    use crate::store::Store;
    use crate::traits::{VectorDocument, VectorIndex};
    use crate::types::{Chunk, EvidenceKind, Source, SourceLocator, SourceStatus};

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
}
