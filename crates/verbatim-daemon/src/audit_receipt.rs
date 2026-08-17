use verbatim_core::api::{
    AuditReceipt, AuditReceiptResult, RetrieveControlsResponse, RetrieveResultResponse,
    AUDIT_RECEIPT_VERSION,
};

/// Build the versioned audit receipt for the final returned results page.
pub(super) fn snapshot(
    embedding_profile_id: &str,
    controls: &crate::EffectiveRetrieveControls,
    results: &[RetrieveResultResponse],
) -> (RetrieveControlsResponse, AuditReceipt) {
    let controls_response = RetrieveControlsResponse {
        fast: controls.fast,
        rerank_enabled: controls.rerank_config.enabled,
        dense_top_k: controls.retrieval_config.dense_top_k,
        bm25_top_k: controls.retrieval_config.bm25_top_k,
        rrf_k: controls.retrieval_config.rrf_k,
        rerank_top_n: controls.rerank_config.top_n,
    };
    let receipt = AuditReceipt {
        version: AUDIT_RECEIPT_VERSION,
        embedding_profile_id: embedding_profile_id.to_string(),
        source_bounded: true,
        controls: controls_response.clone(),
        results: results
            .iter()
            .map(|result| AuditReceiptResult {
                evidence_id: result.evidence_id.clone(),
                text_hash: result.text_hash.clone(),
                source_hash: result.source_hash.clone(),
            })
            .collect(),
    };
    (controls_response, receipt)
}
