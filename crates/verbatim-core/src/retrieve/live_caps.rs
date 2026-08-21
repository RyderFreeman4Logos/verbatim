use anyhow::{anyhow, Result};

use crate::overfetch::{SearchBudget, SearchBudgetFields};

use super::RetrievalPipeline;

impl<'a> RetrievalPipeline<'a> {
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
}
