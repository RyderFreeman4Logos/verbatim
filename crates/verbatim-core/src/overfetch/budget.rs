//! Hard caps for every normal retrieval-orchestration stage.

use serde::{Deserialize, Deserializer, Serialize};

use super::{OverfetchError, OverfetchResult};

/// A retriever whose candidate request is bounded independently.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrieverKind {
    Dense,
    Lexical,
    Exact,
    Graph,
}

impl RetrieverKind {
    /// Every bounded normal-query retriever.
    pub const ALL: [Self; 4] = [Self::Dense, Self::Lexical, Self::Exact, Self::Graph];
}

/// Field bag used to construct and validate a [`SearchBudget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchBudgetFields {
    pub dense_candidate_k: u32,
    pub lexical_candidate_k: u32,
    pub exact_candidate_k: u32,
    pub graph_candidate_k: u32,
    pub fused_pool_size: u32,
    pub rerank_input_size: u32,
    pub final_hydration_list_size: u32,
    pub debug_output_size: u32,
}

/// Hard normal-query limits, enforced before every next retrieval stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct SearchBudget {
    pub dense_candidate_k: u32,
    pub lexical_candidate_k: u32,
    pub exact_candidate_k: u32,
    pub graph_candidate_k: u32,
    pub fused_pool_size: u32,
    pub rerank_input_size: u32,
    pub final_hydration_list_size: u32,
    pub debug_output_size: u32,
}

impl<'de> Deserialize<'de> for SearchBudget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = SearchBudgetFields::deserialize(deserializer)?;
        Self::new(fields).map_err(serde::de::Error::custom)
    }
}

impl SearchBudget {
    /// Builds a budget only when every stage remains positively and monotonically bounded.
    pub fn new(fields: SearchBudgetFields) -> OverfetchResult<Self> {
        let budget = Self {
            dense_candidate_k: fields.dense_candidate_k,
            lexical_candidate_k: fields.lexical_candidate_k,
            exact_candidate_k: fields.exact_candidate_k,
            graph_candidate_k: fields.graph_candidate_k,
            fused_pool_size: fields.fused_pool_size,
            rerank_input_size: fields.rerank_input_size,
            final_hydration_list_size: fields.final_hydration_list_size,
            debug_output_size: fields.debug_output_size,
        };
        budget.validate()?;
        Ok(budget)
    }

    /// Conservative walking-skeleton defaults for callers that need explicit limits.
    pub const fn skeleton_default() -> Self {
        Self {
            dense_candidate_k: 64,
            lexical_candidate_k: 64,
            exact_candidate_k: 32,
            graph_candidate_k: 32,
            fused_pool_size: 128,
            rerank_input_size: 64,
            final_hydration_list_size: 16,
            debug_output_size: 32,
        }
    }

    /// Revalidates fields after decode or before an adapter creates work.
    pub fn validate(&self) -> OverfetchResult<()> {
        if [
            self.dense_candidate_k,
            self.lexical_candidate_k,
            self.exact_candidate_k,
            self.graph_candidate_k,
            self.fused_pool_size,
            self.rerank_input_size,
            self.final_hydration_list_size,
            self.debug_output_size,
        ]
        .contains(&0)
            || self.rerank_input_size > self.fused_pool_size
            || self.final_hydration_list_size > self.rerank_input_size
        {
            return Err(OverfetchError::BudgetExceeded);
        }
        self.total_retriever_candidates()?;
        Ok(())
    }

    /// Candidate cap for one retriever, before it may call a backend.
    pub const fn candidate_k(self, retriever: RetrieverKind) -> u32 {
        match retriever {
            RetrieverKind::Dense => self.dense_candidate_k,
            RetrieverKind::Lexical => self.lexical_candidate_k,
            RetrieverKind::Exact => self.exact_candidate_k,
            RetrieverKind::Graph => self.graph_candidate_k,
        }
    }

    /// Sum of all independently bounded retriever outputs.
    pub fn total_retriever_candidates(self) -> OverfetchResult<u32> {
        self.dense_candidate_k
            .checked_add(self.lexical_candidate_k)
            .and_then(|total| total.checked_add(self.exact_candidate_k))
            .and_then(|total| total.checked_add(self.graph_candidate_k))
            .ok_or(OverfetchError::BudgetExceeded)
    }
}
