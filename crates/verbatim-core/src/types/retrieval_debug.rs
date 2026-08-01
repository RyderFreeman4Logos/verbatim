use serde::{Deserialize, Serialize};

use super::{
    RetrievalDebugEvidencePackMode, RetrievalDenseVectorPath, RetrievalEvidencePackEntry,
    RetrievalFusedHit, RetrievalGraphExpansionDebug, RetrievalRerankDebug, RetrievalStageHit,
};
use crate::retrieval_telemetry::CandidateCounters;

/// Retrieve diagnostic durations in milliseconds.
///
/// `canonical_*` values are nested in `display_evidence_pack_ms` when present;
/// `response_formatting_ms` is measured by the daemon after core retrieval.
/// `dense_vector_search_ms` remains end-to-end and may include wrapper overhead,
/// so it is not defined as the exact sum of the optional vector resource timings.
/// Those timings are `None` when no vector resource is injected.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalLocalSpansMs {
    pub setup_ms: u64,
    pub query_embedding_ms: u64,
    pub dense_vector_search_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_queue_wait_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_service_ms: Option<u64>,
    pub bm25_search_ms: u64,
    pub rrf_fusion_ms: u64,
    pub debug_candidate_pack_ms: u64,
    pub rerank_total_ms: u64,
    pub result_hydration_ms: u64,
    pub graph_expansion_ms: u64,
    pub final_evidence_pack_ms: u64,
    pub display_evidence_pack_ms: u64,
    pub response_formatting_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_support_embedding_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub canonical_display_selection_ms: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrievalDebug {
    #[serde(default)]
    pub dense_vector_path: RetrievalDenseVectorPath,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub query_embedding_latency_ms: Option<u64>,
    #[serde(default)]
    pub local_spans_ms: RetrievalLocalSpansMs,
    /// Fixed-cardinality work counts captured by the shared retrieval pipeline.
    #[serde(default)]
    pub candidate_counters: CandidateCounters,
    #[serde(default)]
    pub evidence_pack_mode: RetrievalDebugEvidencePackMode,
    #[serde(default)]
    pub final_evidence_count: usize,
    #[serde(default)]
    pub display_evidence_count: usize,
    pub bm25_hits: Vec<RetrievalStageHit>,
    pub dense_hits: Vec<RetrievalStageHit>,
    pub rrf_fused_hits: Vec<RetrievalFusedHit>,
    pub graph_expanded_hits: Vec<RetrievalGraphExpansionDebug>,
    pub reranker: RetrievalRerankDebug,
    pub final_evidence_pack: Vec<RetrievalEvidencePackEntry>,
    /// Evidence entries selected for compact no-passage display.
    ///
    /// For canonical multi-locator chunks this may contain a chunk-internal
    /// support unit instead of the chunk's first unit. The score remains the
    /// ranked chunk score; passage rendering uses ranked chunk membership and
    /// structured locators directly.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub display_evidence_pack: Vec<RetrievalEvidencePackEntry>,
}
