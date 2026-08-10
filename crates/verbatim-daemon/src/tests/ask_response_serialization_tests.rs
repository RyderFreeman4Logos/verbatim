#[test]
fn ask_response_omits_retrieval_when_debug_is_off() {
    let response = AskResponse {
        answer: "Answer [E1].".into(),
        generated_interpretation: None,
        citations: Vec::new(),
        verified: false,
        retrieval: None,
        context: None,
        collection_filter: None,
    };

    let encoded = serde_json::to_value(response).unwrap();

    assert_eq!(
        encoded,
        serde_json::json!({
            "answer": "Answer [E1].",
            "citations": [],
            "verified": false,
        })
    );
    assert!(encoded.get("collection_filter").is_none());
}

#[test]
fn ask_response_includes_structured_retrieval_when_requested() {
    let response = AskResponse {
        answer: "Answer [E1].".into(),
        generated_interpretation: None,
        citations: Vec::new(),
        verified: false,
        retrieval: Some(RetrievalDebug {
            dense_vector_path: RetrievalDenseVectorPath::Bm25Only,
            query_embedding_latency_ms: None,
            retrieval_search_sql_statement_count: None,
            retrieval_resource_counters: None,
            local_spans_ms: RetrievalLocalSpansMs::default(),
            candidate_counters: Default::default(),
            evidence_pack_mode: RetrievalDebugEvidencePackMode::Full,
            final_evidence_count: 0,
            display_evidence_count: 0,
            bm25_hits: Vec::new(),
            dense_hits: Vec::new(),
            rrf_fused_hits: Vec::new(),
            graph_expanded_hits: Vec::new(),
            reranker: verbatim_core::types::RetrievalRerankDebug::disabled(),
            final_evidence_pack: Vec::new(),
            display_evidence_pack: Vec::new(),
        }),
        context: None,
        collection_filter: None,
    };

    let encoded = serde_json::to_string(&response).unwrap();

    assert!(encoded.contains("retrieval"));
    assert!(encoded.contains("bm25_hits"));
    assert!(encoded.contains("final_evidence_pack"));
    assert!(encoded.contains("disabled"));
    assert!(!encoded.contains("api_key"));
    assert!(!encoded.contains("secret full raw source text"));
}
