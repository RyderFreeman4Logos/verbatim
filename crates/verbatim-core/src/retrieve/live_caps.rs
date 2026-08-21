use anyhow::{anyhow, Result};

use crate::overfetch::{SearchBudget, SearchBudgetFields};
use crate::types::{ChunkId, RetrievalProvenance, RetrievalResult};

use super::{canonical_multi_evidence_result, RetrievalPipeline};

impl<'a> RetrievalPipeline<'a> {
    pub fn with_canonical_fused_tail(mut self) -> Self {
        self.canonical_fused_tail = true;
        self
    }

    pub(super) fn canonical_display_results(
        &self,
        results: &[RetrievalResult],
        fused: &[(ChunkId, f32)],
    ) -> Result<Vec<RetrievalResult>> {
        if self.canonical_fused_tail && results.iter().any(canonical_multi_evidence_result) {
            self.canonical_debug_results(results, fused)
        } else {
            Ok(results.to_vec())
        }
    }

    pub(super) fn search_budget(&self) -> Result<SearchBudget> {
        let dense_candidate_k = u32::try_from(self.config.dense_top_k.max(1))?;
        let lexical_candidate_k = u32::try_from(self.config.bm25_top_k.max(1))?;
        let final_hydration_list_size = u32::try_from(self.config.default_limit.max(1))?;
        let fused_pool_size = dense_candidate_k
            .max(lexical_candidate_k)
            .max(final_hydration_list_size);
        SearchBudget::new(SearchBudgetFields {
            dense_candidate_k,
            lexical_candidate_k,
            exact_candidate_k: final_hydration_list_size,
            graph_candidate_k: final_hydration_list_size,
            fused_pool_size,
            rerank_input_size: fused_pool_size,
            final_hydration_list_size,
            debug_output_size: final_hydration_list_size,
        })
        .map_err(|error| anyhow!("invalid retrieval search budget: {error}"))
    }

    pub(super) fn canonical_debug_results(
        &self,
        results: &[RetrievalResult],
        fused: &[(ChunkId, f32)],
    ) -> Result<Vec<RetrievalResult>> {
        let mut seen = results
            .iter()
            .map(|result| result.chunk_id.clone())
            .collect::<std::collections::HashSet<_>>();
        let extra = fused
            .iter()
            .enumerate()
            .filter(|(_, (chunk_id, _))| seen.insert(chunk_id.clone()))
            .collect::<Vec<_>>();
        if extra.is_empty() {
            return Ok(results.to_vec());
        }

        let chunk_ids = extra
            .iter()
            .map(|(_, (chunk_id, _))| (*chunk_id).clone())
            .collect::<Vec<_>>();
        let chunks = self.store.get_chunks(&chunk_ids)?;
        let parent_ids = chunks
            .values()
            .filter_map(|chunk| {
                chunk
                    .as_ref()
                    .ok()
                    .and_then(|chunk| chunk.parent_chunk_id.clone())
            })
            .collect::<Vec<_>>();
        let parents = self.store.get_chunks(&parent_ids)?;
        let mut canonical_results = results.to_vec();

        for (rank, (chunk_id, score)) in extra {
            let Some(Ok(chunk)) = chunks.get(chunk_id) else {
                continue;
            };
            let chunk = chunk.clone();
            let provenance =
                RetrievalProvenance::seed(rank + 1, chunk.id.clone(), chunk.source_id.clone());
            let parent_chunk = chunk
                .parent_chunk_id
                .as_ref()
                .and_then(|parent_id| parents.get(parent_id))
                .and_then(|parent| parent.as_ref().ok())
                .cloned();
            canonical_results.push(self.result_for_chunk_with_parent(
                chunk,
                parent_chunk,
                *score,
                provenance,
            )?);
        }

        Ok(canonical_results)
    }
}
