use super::*;
use crate::retrieval_telemetry::RetrievalResourceCounters;

#[path = "types_retrieval_provenance_wire_decode.rs"]
mod retrieval_provenance_wire_decode_tests;

#[test]
fn mvp_regression_source_ids_include_path_hash_to_avoid_stem_collisions() {
    let tmp = tempfile::tempdir().unwrap();
    let left_dir = tmp.path().join("left");
    let right_dir = tmp.path().join("right");
    std::fs::create_dir_all(&left_dir).unwrap();
    std::fs::create_dir_all(&right_dir).unwrap();
    let left = left_dir.join("notes.md");
    let right = right_dir.join("notes.md");
    std::fs::write(&left, "left").unwrap();
    std::fs::write(&right, "right").unwrap();

    let left_id = SourceId::from_path(&left);
    let right_id = SourceId::from_path(&right);

    assert_ne!(left_id, right_id);
    assert!(left_id.0.starts_with("notes-"));
    assert!(right_id.0.starts_with("notes-"));
}

#[test]
fn pdf_image_locators_include_image_index_and_bbox() {
    let locator = SourceLocator::PdfImage {
        page: 84,
        image_index: 2,
        bbox: Some(BBox {
            x0: 1.0,
            y0: 2.0,
            x1: 3.0,
            y1: 4.0,
        }),
    };

    assert_eq!(
        locator.to_string(),
        "PDF p.84, image 2, bbox=[1.00,2.00,3.00,4.00]"
    );
}

#[test]
fn pdf_image_ids_are_stable_for_same_hash_and_bbox() {
    let source_id = SourceId("src".into());
    let bbox = BBox {
        x0: 1.0,
        y0: 2.0,
        x1: 3.0,
        y1: 4.0,
    };

    let first = ImageId::for_pdf_image(&source_id, 1, Some(&bbox), "hash");
    let second = ImageId::for_pdf_image(&source_id, 1, Some(&bbox), "hash");
    let different = ImageId::for_pdf_image(&source_id, 2, Some(&bbox), "hash");

    assert_eq!(first, second);
    assert_ne!(first, different);
}

#[test]
fn retrieval_debug_serializes_without_raw_text_or_secrets() {
    let resources = RetrievalResourceCounters::observed(Some(0), Some(3), Some(0), Some(4_096));
    let debug = RetrievalDebug {
        dense_vector_path: RetrievalDenseVectorPath::ResidentHnsw,
        query_embedding_latency_ms: None,
        retrieval_search_sql_statement_count: Some(0),
        retrieval_resource_counters: Some(resources),
        local_spans_ms: RetrievalLocalSpansMs {
            setup_ms: 1,
            query_embedding_ms: 2,
            dense_vector_search_ms: 3,
            vector_queue_wait_ms: Some(4),
            vector_service_ms: Some(5),
            bm25_search_ms: 6,
            rrf_fusion_ms: 7,
            debug_candidate_pack_ms: 8,
            rerank_total_ms: 9,
            result_hydration_ms: 10,
            graph_expansion_ms: 11,
            final_evidence_pack_ms: 12,
            display_evidence_pack_ms: 13,
            response_formatting_ms: 14,
            canonical_support_embedding_ms: Some(15),
            canonical_display_selection_ms: Some(16),
        },
        candidate_counters: Default::default(),
        bm25_hits: vec![RetrievalStageHit {
            rank: 1,
            chunk_id: ChunkId("chunk-1".into()),
            source_id: Some(SourceId("src-1".into())),
            score: 4.2,
            evidence_ids: vec![EvidenceId("ev-1".into())],
        }],
        dense_hits: Vec::new(),
        rrf_fused_hits: vec![RetrievalFusedHit {
            rank: 1,
            chunk_id: ChunkId("chunk-1".into()),
            source_id: Some(SourceId("src-1".into())),
            score: 0.03,
            dense_rank: None,
            bm25_rank: Some(1),
            evidence_ids: vec![EvidenceId("ev-1".into())],
        }],
        graph_expanded_hits: vec![RetrievalGraphExpansionDebug {
            result_rank: 2,
            seed_rank: 1,
            seed_chunk_id: ChunkId("chunk-1".into()),
            seed_source_id: SourceId("src-1".into()),
            expanded_chunk_id: ChunkId("chunk-2".into()),
            expanded_source_id: SourceId("src-1".into()),
            score: 0.01,
            hop_distance: 1,
            path: vec![GraphExpansionStep {
                edge_type: EdgeType::Next,
                from_node_id: GraphNodeId("node-1".into()),
                to_node_id: GraphNodeId("node-2".into()),
                direction: GraphTraversalDirection::Outgoing,
            }],
            reason: "included_by_configured_graph_expansion".into(),
        }],
        reranker: RetrievalRerankDebug::skipped("disabled"),
        evidence_pack_mode: RetrievalDebugEvidencePackMode::Full,
        final_evidence_count: 1,
        display_evidence_count: 1,
        final_evidence_pack: vec![RetrievalEvidencePackEntry {
            label: "E1".into(),
            result_rank: 1,
            chunk_id: ChunkId("chunk-1".into()),
            score: 0.03,
            evidence_id: EvidenceId("ev-1".into()),
            source_id: SourceId("src-1".into()),
            role: RetrievalEvidenceRole::OriginalText,
            kind: EvidenceKind::Text,
            derived_from: None,
            locator: RetrievalLocatorDebug {
                display: "/tmp/doc.txt L1".into(),
                structured: SourceLocator::Document {
                    path_or_url: "/tmp/doc.txt".into(),
                    line_start: 1,
                    line_end: None,
                },
            },
            provenance: RetrievalProvenance::seed(
                1,
                ChunkId("chunk-1".into()),
                SourceId("src-1".into()),
            ),
        }],
        display_evidence_pack: Vec::new(),
    };

    let encoded = serde_json::to_string(&debug).unwrap();
    assert!(encoded.contains("local_spans_ms"));
    assert!(encoded.contains("dense_vector_search_ms"));
    assert!(encoded.contains("vector_queue_wait_ms"));
    assert!(encoded.contains("vector_service_ms"));
    assert!(encoded.contains("response_formatting_ms"));
    assert!(encoded.contains("canonical_display_selection_ms"));
    assert!(encoded.contains("\"retrieval_search_sql_statement_count\":0"));
    assert!(encoded.contains("\"storage_read_bytes\":4096"));
    assert!(encoded.contains("evidence_pack_mode"));
    assert!(encoded.contains("final_evidence_count"));
    assert!(encoded.contains("display_evidence_count"));
    assert!(encoded.contains("bm25_hits"));
    assert!(encoded.contains("graph_expanded_hits"));
    assert!(encoded.contains("final_evidence_pack"));
    assert!(encoded.contains("disabled"));
    assert!(!encoded.contains("api_key"));
    assert!(!encoded.contains("secret full raw source text"));

    let decoded: RetrievalDebug = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, debug);

    let mut legacy_payload = serde_json::to_value(&debug).unwrap();
    let legacy_fields = legacy_payload.as_object_mut().unwrap();
    legacy_fields.remove("retrieval_search_sql_statement_count");
    legacy_fields.remove("retrieval_resource_counters");
    let legacy_spans = legacy_payload["local_spans_ms"].as_object_mut().unwrap();
    legacy_spans.remove("vector_queue_wait_ms");
    legacy_spans.remove("vector_service_ms");
    let decoded_legacy: RetrievalDebug = serde_json::from_value(legacy_payload).unwrap();
    assert_eq!(decoded_legacy.local_spans_ms.vector_queue_wait_ms, None);
    assert_eq!(decoded_legacy.local_spans_ms.vector_service_ms, None);
    assert_eq!(decoded_legacy.retrieval_search_sql_statement_count, None);
    assert_eq!(decoded_legacy.retrieval_resource_counters, None);

    let mut unavailable = debug;
    unavailable.retrieval_search_sql_statement_count = None;
    unavailable.retrieval_resource_counters = None;
    let unavailable_payload = serde_json::to_value(unavailable).unwrap();
    let unavailable_fields = unavailable_payload.as_object().unwrap();
    assert!(!unavailable_fields.contains_key("retrieval_search_sql_statement_count"));
    assert!(!unavailable_fields.contains_key("retrieval_resource_counters"));
}

#[test]
fn rerank_debug_metadata_serializes_without_request_text_or_secrets() {
    let mut debug = RetrievalRerankDebug::fallback("vllm", "rerank-model", 1, 1, "http_status_400");
    debug.capability = Some(RetrievalRerankCapabilityDebug {
        state: RetrievalRerankCapabilityState::Refreshed,
        max_context_tokens: Some(512),
        max_candidates: Some(2),
        max_documents: Some(2),
        max_document_chars: Some(1024),
        max_payload_chars: Some(4096),
        reason: Some("capability_absent".into()),
        retried_after_context_limit: true,
    });
    debug.request = Some(RetrievalRerankRequestDebug {
        candidate_count: 1,
        document_char_limit: 768,
        top_n: 1,
    });

    let encoded = serde_json::to_string(&debug).unwrap();

    assert!(encoded.contains("refreshed"));
    assert!(encoded.contains("max_document_chars"));
    assert!(encoded.contains("document_char_limit"));
    assert!(!encoded.contains("Authorization"));
    assert!(!encoded.contains("Bearer"));
    assert!(!encoded.contains("api_key"));
    assert!(!encoded.contains("secret query token=fixture-query"));
    assert!(!encoded.contains("secret document body"));
}

#[test]
fn reference_component_serializes_with_optional_ordinal() {
    let with_ordinal = ReferenceComponent {
        level: "verse".into(),
        value: "16".into(),
        ordinal: Some(16),
    };
    let without_ordinal = ReferenceComponent {
        level: "book".into(),
        value: "John".into(),
        ordinal: None,
    };

    let encoded_with = serde_json::to_string(&with_ordinal).unwrap();
    assert!(encoded_with.contains("\"ordinal\":16"));

    let encoded_without = serde_json::to_string(&without_ordinal).unwrap();
    assert!(!encoded_without.contains("ordinal"));
}

#[test]
fn canonical_locator_round_trips_through_json() {
    let locator = CanonicalLocator {
        profile_id: "bible".into(),
        work_id: "CSB".into(),
        version_id: Some("digital-edition-2017".into()),
        canon_id: None,
        versification_id: None,
        start: vec![
            ReferenceComponent {
                level: "book".into(),
                value: "John".into(),
                ordinal: Some(43),
            },
            ReferenceComponent {
                level: "chapter".into(),
                value: "3".into(),
                ordinal: Some(3),
            },
            ReferenceComponent {
                level: "verse".into(),
                value: "16".into(),
                ordinal: Some(16),
            },
        ],
        end: None,
        display: "John 3:16".into(),
        normalized: "john:3:16".into(),
        backing_selectors: vec![BackingSelector::LineRange { start: 1, end: 1 }],
    };

    let encoded = serde_json::to_string(&locator).unwrap();
    let decoded: CanonicalLocator = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, locator);
}

#[test]
fn canonical_source_locator_round_trips_through_json() {
    let locator = CanonicalLocator::single_unit(
        "bible",
        "CSB",
        vec![
            ReferenceComponent {
                level: "book".into(),
                value: "John".into(),
                ordinal: Some(43),
            },
            ReferenceComponent {
                level: "chapter".into(),
                value: "3".into(),
                ordinal: Some(3),
            },
            ReferenceComponent {
                level: "verse".into(),
                value: "16".into(),
                ordinal: Some(16),
            },
        ],
        "John 3:16".into(),
        "john:3:16".into(),
    );
    let source_locator = SourceLocator::Canonical { locator };

    let encoded = serde_json::to_string(&source_locator).unwrap();
    assert!(encoded.contains("\"type\":\"Canonical\""));

    let decoded: SourceLocator = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, source_locator);
}

#[test]
fn canonical_locator_display_formats_citation() {
    let locator = CanonicalLocator::single_unit(
        "bible",
        "CSB",
        vec![ReferenceComponent {
            level: "book".into(),
            value: "John".into(),
            ordinal: None,
        }],
        "John 3:16".into(),
        "john:3:16".into(),
    );
    let source_locator = SourceLocator::Canonical { locator };
    assert_eq!(source_locator.to_string(), "John 3:16");
}

#[test]
fn backing_selector_line_range_round_trips() {
    let selector = BackingSelector::LineRange { start: 5, end: 10 };
    let encoded = serde_json::to_string(&selector).unwrap();
    assert!(encoded.contains("\"type\":\"LineRange\""));
    assert!(encoded.contains("\"start\":5"));
    assert!(encoded.contains("\"end\":10"));

    let decoded: BackingSelector = serde_json::from_str(&encoded).unwrap();
    assert_eq!(decoded, selector);
}
