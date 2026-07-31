use serde::{Deserialize, Serialize};

use super::{
    RetrievalDebugEvidencePackMode, RetrievalDenseVectorPath, RetrievalEvidencePackEntry,
    RetrievalFusedHit, RetrievalGraphExpansionDebug, RetrievalLocalSpansMs, RetrievalRerankDebug,
    RetrievalStageHit,
};
use crate::retrieval_telemetry::CandidateCounters;

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
