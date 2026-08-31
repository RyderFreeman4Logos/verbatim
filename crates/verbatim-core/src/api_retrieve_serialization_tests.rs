#[test]
fn retrieve_result_omits_structured_locator_until_requested() {
    let response = RetrieveResponse {
        task_id: "task-1".into(),
        query: "What is cited?".into(),
        text_taxonomy: ResponseTextTaxonomy::retrieve_response(),
        source_id: None,
        collection_filter: None,
        embedding_profile_id: "default".into(),
        query_plan: None,
        evidence_pack: None,
        generation: None,
        limit: 12,
        page_size: 1,
        page: 1,
        total_results: 1,
        returned_results: 1,
        source_bounded: true,
        controls: RetrieveControlsResponse {
            fast: false,
            rerank_enabled: false,
            dense_top_k: 80,
            bm25_top_k: 50,
            rrf_k: 60,
            rerank_top_n: 12,
        },
        audit_receipt: AuditReceipt {
            version: AUDIT_RECEIPT_VERSION,
            embedding_profile_id: "default".into(),
            source_bounded: true,
            controls: RetrieveControlsResponse {
                fast: false,
                rerank_enabled: false,
                dense_top_k: 80,
                bm25_top_k: 50,
                rrf_k: 60,
                rerank_top_n: 12,
            },
            results: vec![AuditReceiptResult {
                evidence_id: "ev-1".into(),
                text_hash: "verified-text-hash".into(),
                source_hash: "persisted-source-hash".into(),
            }],
        },
        timings: vec![RetrieveTimingResponse {
            phase: "retrieval".into(),
            duration_ms: 7,
        }],
        results: vec![RetrieveResultResponse {
            index: 0,
            rank: 1,
            label: "E1".into(),
            evidence_id: "ev-1".into(),
            text_hash: "verified-text-hash".into(),
            source_id: "src-1".into(),
            source_hash: "persisted-source-hash".into(),
            source_path: Some("/tmp/doc.md".into()),
            collections: Vec::new(),
            chunk_id: "chunk-1".into(),
            kind: "text".into(),
            role: "original_text".into(),
            score: 0.03,
            locator: "/tmp/doc.md L1".into(),
            structured_locator: None,
            provenance: None,
            derived_from: None,
            snippet: "compact cited text".into(),
        }],
        debug: None,
    };

    let encoded = serde_json::to_string(&response).unwrap();

    assert!(encoded.contains("\"locator\""));
    assert!(encoded.contains("\"source_bounded\":true"));
    assert!(encoded.contains("\"text_hash\":\"verified-text-hash\""));
    assert!(encoded.contains("\"source_hash\":\"persisted-source-hash\""));
    assert!(!encoded.contains("structured_locator"));
    assert!(!encoded.contains("provenance"));
    let encoded_value = serde_json::to_value(&response).unwrap();
    assert!(encoded_value.get("debug").is_none());

    let mut requested = response;
    requested.results[0].structured_locator = Some(SourceLocator::Document {
        path_or_url: "/tmp/doc.md".into(),
        line_start: 1,
        line_end: Some(1),
    });
    requested.text_taxonomy =
        ResponseTextTaxonomy::retrieve_response_with_results(&requested.results);
    let requested_encoded = serde_json::to_string(&requested).unwrap();
    assert!(requested_encoded.contains("structured_locator"));
}

include!("api_reindex_result_identity_wire_tests.rs");
include!("api_collection_sync_result_identity_wire_tests.rs");
include!("api_collection_watchers_status_result_identity_wire_tests.rs");
