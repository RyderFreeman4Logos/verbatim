use serde::{Deserialize, Serialize};

use crate::retrieval_telemetry::CandidateCounters;
use crate::types::{RetrievalDenseVectorPath, RetrievalRerankStatus};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrieveTaskProfile {
    #[serde(default)]
    pub candidate_counters: CandidateCounters,
    pub dense: RetrieveDenseStageProfile,
    pub bm25: RetrieveStageProfile,
    pub fusion: RetrieveStageProfile,
    pub rerank: RetrieveRerankStageProfile,
    pub evidence: RetrieveEvidenceStageProfile,
    pub display: RetrieveDisplayStageProfile,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrieveDenseStageProfile {
    pub path: RetrievalDenseVectorPath,
    pub candidate_count: usize,
    pub local_ms: u64,
    pub query_embedding_ms: u64,
    pub endpoint_latency_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrieveStageProfile {
    pub candidate_count: usize,
    pub local_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrieveRerankStageProfile {
    pub status: RetrievalRerankStatus,
    pub reason: Option<String>,
    pub input_count: Option<usize>,
    pub configured_top_n: usize,
    pub effective_top_n: Option<usize>,
    pub output_count: usize,
    pub local_ms: u64,
    pub endpoint_latency_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrieveEvidenceStageProfile {
    pub result_count: usize,
    pub graph_expanded_count: usize,
    pub final_count: usize,
    pub display_count: usize,
    pub result_hydration_ms: u64,
    pub graph_expansion_ms: u64,
    pub final_pack_ms: u64,
    pub display_pack_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrieveDisplayStageProfile {
    pub returned_count: usize,
    pub response_formatting_ms: u64,
    pub canonical_support_embedding_ms: Option<u64>,
    pub canonical_display_selection_ms: Option<u64>,
    pub canonical_selected_count: Option<usize>,
}
