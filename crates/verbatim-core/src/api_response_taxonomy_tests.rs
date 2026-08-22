use std::collections::HashSet;

use super::*;

#[test]
fn ask_response_taxonomy_classifies_generated_and_source_citation_text_by_kind() {
    let citations = vec![
        CitationResponse {
            label: "E1".into(),
            evidence_id: "ev-generated".into(),
            kind: "generated".into(),
            derived_from: None,
            collections: Vec::new(),
            locator: "generated".into(),
            text_preview: "Generated citation text.".into(),
        },
        CitationResponse {
            label: "E2".into(),
            evidence_id: "ev-source".into(),
            kind: "original_text".into(),
            derived_from: None,
            collections: Vec::new(),
            locator: "doc.md L1".into(),
            text_preview: "Persisted source quote.".into(),
        },
    ];
    let response = AskResponse {
        answer: "Legacy generated answer.".into(),
        answer_kind: AnswerKind::GeneratedInterpretation,
        text_taxonomy: ResponseTextTaxonomy::ask_response_with_citations(&citations),
        generated_interpretation: Some(GeneratedInterpretationResponse {
            text: "Generated interpretation.".into(),
        }),
        citations,
        verified: false,
        retrieval: None,
        context: None,
        collection_filter: None,
    };

    let encoded = serde_json::to_value(response).unwrap();
    let fields = encoded["text_taxonomy"]["fields"]
        .as_array()
        .expect("published text taxonomy");
    let plane_for = |field| {
        fields
            .iter()
            .find(|entry| entry["field"] == field)
            .map(|entry| entry["plane"].clone())
            .expect("field classification")
    };

    assert_eq!(
        plane_for("answer"),
        serde_json::json!("generated_interpretation")
    );
    assert_eq!(
        plane_for("citations[].label"),
        serde_json::json!("deterministic_interface_text")
    );
    assert_eq!(
        plane_for("citations[0].text_preview"),
        serde_json::json!("generated_interpretation")
    );
    assert_eq!(
        plane_for("citations[1].text_preview"),
        serde_json::json!("evidence")
    );
}

#[test]
fn retrieve_response_taxonomy_classifies_generated_and_source_result_text_by_role() {
    let results = vec![
        RetrieveResultResponse {
            index: 0,
            rank: 1,
            label: "E1".into(),
            evidence_id: "ev-generated".into(),
            text_hash: "generated-hash".into(),
            source_id: "src".into(),
            source_hash: "source-hash".into(),
            source_path: None,
            collections: Vec::new(),
            chunk_id: "chunk-generated".into(),
            kind: "generated".into(),
            role: "image_caption_generated".into(),
            score: 1.0,
            locator: "generated".into(),
            structured_locator: None,
            provenance: None,
            derived_from: None,
            snippet: "Generated result text.".into(),
        },
        RetrieveResultResponse {
            index: 1,
            rank: 2,
            label: "E2".into(),
            evidence_id: "ev-source".into(),
            text_hash: "source-text-hash".into(),
            source_id: "src".into(),
            source_hash: "source-hash".into(),
            source_path: None,
            collections: Vec::new(),
            chunk_id: "chunk-source".into(),
            kind: "text".into(),
            role: "original_text".into(),
            score: 0.9,
            locator: "doc.md L1".into(),
            structured_locator: None,
            provenance: None,
            derived_from: None,
            snippet: "Persisted result text.".into(),
        },
    ];
    let response = RetrieveResponse {
        task_id: "task-1".into(),
        query: "question".into(),
        text_taxonomy: ResponseTextTaxonomy::retrieve_response_with_results(&results),
        source_id: Some("src".into()),
        collection_filter: None,
        embedding_profile_id: "default".into(),
        limit: 2,
        page_size: 2,
        page: 1,
        total_results: 2,
        returned_results: 2,
        source_bounded: true,
        controls: RetrieveControlsResponse {
            fast: false,
            rerank_enabled: false,
            dense_top_k: 1,
            bm25_top_k: 1,
            rrf_k: 1,
            rerank_top_n: 1,
        },
        audit_receipt: AuditReceipt {
            version: 1,
            embedding_profile_id: "default".into(),
            source_bounded: true,
            controls: RetrieveControlsResponse {
                fast: false,
                rerank_enabled: false,
                dense_top_k: 1,
                bm25_top_k: 1,
                rrf_k: 1,
                rerank_top_n: 1,
            },
            results: Vec::new(),
        },
        timings: Vec::new(),
        results,
        debug: None,
    };

    let encoded = serde_json::to_value(response).unwrap();
    let fields = encoded["text_taxonomy"]["fields"]
        .as_array()
        .expect("published text taxonomy");
    let plane_for = |field| {
        fields
            .iter()
            .find(|entry| entry["field"] == field)
            .map(|entry| entry["plane"].clone())
            .expect("field classification")
    };

    assert_eq!(
        plane_for("results[0].snippet"),
        serde_json::json!("generated_interpretation")
    );
    assert_eq!(
        plane_for("results[1].snippet"),
        serde_json::json!("evidence")
    );
}

#[test]
fn legacy_evidence_response_without_taxonomy_uses_source_bounded_plane() {
    let response: EvidenceResponse = serde_json::from_str(include_str!(
        "fixtures/legacy_evidence_response_without_taxonomy.json"
    ))
    .unwrap();

    assert!(!response.source_bounded);
    for field in ["text", "heading_path[]"] {
        let taxonomy = response
            .text_taxonomy
            .fields
            .iter()
            .find(|entry| entry.field == field)
            .expect("legacy field classification");
        assert_eq!(taxonomy.plane, OutputTextPlane::GeneratedInterpretation);
    }
}

#[test]
fn response_text_taxonomy_round_trips_all_four_planes() {
    let taxonomy = ResponseTextTaxonomy::ask_response();
    let encoded = serde_json::to_value(&taxonomy).unwrap();
    let decoded: ResponseTextTaxonomy = serde_json::from_value(encoded).unwrap();

    assert_eq!(decoded, taxonomy);
    assert_eq!(taxonomy.version, 1);
    for plane in [
        OutputTextPlane::Evidence,
        OutputTextPlane::Metadata,
        OutputTextPlane::DeterministicInterfaceText,
        OutputTextPlane::GeneratedInterpretation,
    ] {
        assert!(taxonomy.fields.iter().any(|field| field.plane == plane));
    }
}

#[test]
fn response_taxonomy_paths_resolve_to_serialized_text_leaves() {
    let ask: AskResponse = serde_json::from_value(serde_json::json!({
        "answer": "answer",
        "answer_kind": "generated_interpretation",
        "generated_interpretation": {"text": "generated"},
        "citations": [{
            "label": "E1",
            "evidence_id": "ev-1",
            "kind": "text",
            "derived_from": "ev-0",
            "collections": [{
                "collection_id": "col-1",
                "name": "docs",
                "logical_path": "docs",
                "source_path": "/tmp/doc.md",
                "member_updated_at": "2026-08-22T00:00:00Z"
            }, {
                "collection_id": "col-2",
                "name": "papers",
                "logical_path": "papers",
                "source_path": "/tmp/paper.md",
                "member_updated_at": "2026-08-21T00:00:00Z"
            }],
            "locator": "doc.md L1",
            "text_preview": "quote"
        }, {
            "label": "E2",
            "evidence_id": "ev-2",
            "kind": "text",
            "derived_from": null,
            "collections": [],
            "locator": "paper.md L2",
            "text_preview": "second quote"
        }],
        "verified": false,
        "collection_filter": {
            "requested": {
                "collection_ids": ["col-1", "col-2"],
                "names": ["docs", "papers"],
                "require_fresh": false
            },
            "union_source_count": 1,
            "applied": [{
                "collection_id": "col-1",
                "name": "docs",
                "member_count": 1,
                "indexed_member_count": 1,
                "stale_member_count": 0,
                "last_synced_at": "2026-08-22T00:00:00Z",
                "stale": false
            }, {
                "collection_id": "col-2",
                "name": "papers",
                "member_count": 2,
                "indexed_member_count": 2,
                "stale_member_count": 0,
                "last_synced_at": "2026-08-21T00:00:00Z",
                "stale": false
            }],
            "warnings": ["none", "stale"],
            "stale": false
        }
    }))
    .unwrap();
    let retrieve: RetrieveResponse = serde_json::from_value(serde_json::json!({
        "task_id": "task-1",
        "query": "question",
        "source_id": "src-1",
        "collection_filter": {
            "requested": {
                "collection_ids": ["col-1", "col-2"],
                "names": ["docs", "papers"],
                "require_fresh": false
            },
            "union_source_count": 2,
            "applied": [{
                "collection_id": "col-1",
                "name": "docs",
                "member_count": 1,
                "indexed_member_count": 1,
                "stale_member_count": 0,
                "last_synced_at": "2026-08-22T00:00:00Z",
                "stale": false
            }, {
                "collection_id": "col-2",
                "name": "papers",
                "member_count": 1,
                "indexed_member_count": 1,
                "stale_member_count": 0,
                "last_synced_at": "2026-08-21T00:00:00Z",
                "stale": false
            }],
            "warnings": ["none", "stale"],
            "stale": false
        },
        "embedding_profile_id": "default",
        "limit": 1,
        "page_size": 1,
        "page": 1,
        "total_results": 1,
        "returned_results": 1,
        "source_bounded": true,
        "controls": {
            "fast": false,
            "rerank_enabled": false,
            "dense_top_k": 1,
            "bm25_top_k": 1,
            "rrf_k": 1,
            "rerank_top_n": 1
        },
        "audit_receipt": {
            "version": 1,
            "embedding_profile_id": "default",
            "source_bounded": true,
            "controls": {
                "fast": false,
                "rerank_enabled": false,
                "dense_top_k": 1,
                "bm25_top_k": 1,
                "rrf_k": 1,
                "rerank_top_n": 1
            },
            "results": [{
                "evidence_id": "ev-1",
                "text_hash": "text-hash",
                "source_hash": "source-hash"
            }, {
                "evidence_id": "ev-2",
                "text_hash": "text-hash-2",
                "source_hash": "source-hash-2"
            }]
        },
        "timings": [{"phase": "retrieval", "duration_ms": 1}],
        "results": [{
            "index": 0,
            "rank": 1,
            "label": "E1",
            "evidence_id": "ev-1",
            "text_hash": "text-hash",
            "source_id": "src-1",
            "source_hash": "source-hash",
            "source_path": "/tmp/doc.md",
            "collections": [{
                "collection_id": "col-1",
                "name": "docs",
                "logical_path": "docs",
                "source_path": "/tmp/doc.md",
                "member_updated_at": "2026-08-22T00:00:00Z"
            }, {
                "collection_id": "col-2",
                "name": "papers",
                "logical_path": "papers",
                "source_path": "/tmp/paper.md",
                "member_updated_at": "2026-08-21T00:00:00Z"
            }],
            "chunk_id": "chunk-1",
            "kind": "text",
            "role": "original_text",
            "score": 1.0,
            "locator": "doc.md L1",
            "structured_locator": {
                "type": "Document",
                "path_or_url": "/tmp/doc.md",
                "line_start": 1,
                "line_end": 1
            },
            "provenance": {
                "origin": "seed",
                "result_rank": 1,
                "seed_rank": 1,
                "seed_chunk_id": "chunk-1",
                "seed_source_id": "src-1",
                "hop_distance": 0,
                "graph_path": []
            },
            "derived_from": "ev-0",
            "snippet": "quote"
        }, {
            "index": 1,
            "rank": 2,
            "label": "E2",
            "evidence_id": "ev-2",
            "text_hash": "text-hash-2",
            "source_id": "src-2",
            "source_hash": "source-hash-2",
            "source_path": "/tmp/paper.md",
            "collections": [{
                "collection_id": "col-2",
                "name": "papers",
                "logical_path": "papers",
                "source_path": "/tmp/paper.md",
                "member_updated_at": "2026-08-21T00:00:00Z"
            }],
            "chunk_id": "chunk-2",
            "kind": "text",
            "role": "original_text",
            "score": 0.9,
            "locator": "paper.md L2",
            "derived_from": null,
            "snippet": "second quote"
        }]
    }))
    .unwrap();
    let evidence: EvidenceResponse = serde_json::from_value(serde_json::json!({
        "id": "ev-1",
        "source_id": "src-1",
        "source_hash": "source-hash",
        "source_bounded": true,
        "text_hash": "text-hash",
        "kind": "text",
        "derived_from": "ev-0",
        "locator": "doc.md L1",
        "structured_locator": {
            "type": "Document",
            "path_or_url": "/tmp/doc.md",
            "line_start": 1,
            "line_end": 1
        },
        "text": "quote",
        "heading_path": ["Heading"],
        "language": "en",
        "position": 0,
        "image_artifact": {
            "image_id": "img-1",
            "path": "/tmp/doc-image.png",
            "content_hash": "image-hash",
            "mime_type": "image/png",
            "width": 640,
            "height": 480,
            "page": 1,
            "image_index": 1,
            "bbox": [1.0, 2.0, 3.0, 4.0]
        }
    }))
    .unwrap();

    for (name, response) in [
        ("ask", serde_json::to_value(ask).unwrap()),
        ("retrieve", serde_json::to_value(retrieve).unwrap()),
        ("evidence", serde_json::to_value(evidence).unwrap()),
    ] {
        assert_taxonomy_paths_resolve(name, &response);
    }
}

fn assert_taxonomy_paths_resolve(name: &str, response: &serde_json::Value) {
    let fields = response["text_taxonomy"]["fields"]
        .as_array()
        .expect("published text taxonomy");
    let mut seen = HashSet::new();
    for field in fields {
        let path = field["field"].as_str().expect("taxonomy field path");
        assert!(seen.insert(path), "{name} duplicate taxonomy path {path:?}");
    }

    let mut leaves = Vec::new();
    collect_string_leaves(response, String::new(), &mut leaves);
    for leaf in &leaves {
        let matches = fields
            .iter()
            .filter(|field| {
                let path = field["field"].as_str().expect("taxonomy field path");
                taxonomy_path_matches(path, &leaf)
            })
            .count();
        assert_eq!(
            matches, 1,
            "{name} leaf {leaf:?} has {matches} taxonomy entries"
        );
    }
    for field in fields {
        let path = field["field"].as_str().expect("taxonomy field path");
        let matches = leaves
            .iter()
            .filter(|leaf| taxonomy_path_matches(path, leaf))
            .count();
        assert!(
            matches > 0,
            "{name} taxonomy path {path:?} resolves to no serialized string leaf"
        );
    }
}

fn collect_string_leaves(value: &serde_json::Value, path: String, leaves: &mut Vec<String>) {
    match value {
        serde_json::Value::Object(fields) => {
            for (key, value) in fields {
                let child_path = if path.is_empty() {
                    key.clone()
                } else {
                    format!("{path}.{key}")
                };
                if child_path == "text_taxonomy" || child_path.starts_with("text_taxonomy.") {
                    continue;
                }
                collect_string_leaves(value, child_path, leaves);
            }
        }
        serde_json::Value::Array(values) => {
            for (index, value) in values.iter().enumerate() {
                collect_string_leaves(value, format!("{path}[{index}]"), leaves);
            }
        }
        serde_json::Value::String(_) => leaves.push(path),
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

fn taxonomy_path_matches(pattern: &str, concrete: &str) -> bool {
    if pattern.ends_with(".text_preview") || pattern.ends_with(".snippet") {
        pattern == concrete
    } else {
        normalize_array_indices(pattern) == normalize_array_indices(concrete)
    }
}

fn normalize_array_indices(path: &str) -> String {
    let mut normalized = String::new();
    let mut chars = path.chars();
    while let Some(character) = chars.next() {
        if character != '[' {
            normalized.push(character);
            continue;
        }
        let mut index = String::new();
        for character in chars.by_ref() {
            if character == ']' {
                break;
            }
            index.push(character);
        }
        if index.is_empty() || index.chars().all(|character| character.is_ascii_digit()) {
            normalized.push_str("[]");
        } else {
            normalized.push('[');
            normalized.push_str(&index);
            normalized.push(']');
        }
    }
    normalized
}

#[test]
fn compact_ask_taxonomy_omits_absent_optional_paths() {
    let response: AskResponse = serde_json::from_value(serde_json::json!({
        "answer": "Answer [E1].",
        "answer_kind": "evidence_only",
        "citations": [],
        "verified": false
    }))
    .unwrap();
    let encoded = serde_json::to_value(response).unwrap();
    let fields = encoded["text_taxonomy"]["fields"]
        .as_array()
        .expect("published text taxonomy");

    assert!(!fields
        .iter()
        .any(|field| field["field"] == "generated_interpretation.text"));
    assert_taxonomy_paths_resolve("compact ask", &encoded);
}

#[test]
fn taxonomy_classifies_requested_ask_retrieval_debug() {
    let response = AskResponse {
        answer: "Answer [E1].".into(),
        answer_kind: AnswerKind::EvidenceOnly,
        text_taxonomy: ResponseTextTaxonomy::ask_response(),
        generated_interpretation: None,
        citations: Vec::new(),
        verified: false,
        retrieval: Some(crate::types::RetrievalDebug {
            dense_vector_path: crate::types::RetrievalDenseVectorPath::Bm25Only,
            query_embedding_latency_ms: None,
            retrieval_search_sql_statement_count: None,
            retrieval_resource_counters: None,
            local_spans_ms: crate::types::RetrievalLocalSpansMs::default(),
            candidate_counters: Default::default(),
            evidence_pack_mode: crate::types::RetrievalDebugEvidencePackMode::Full,
            final_evidence_count: 0,
            display_evidence_count: 0,
            bm25_hits: Vec::new(),
            dense_hits: Vec::new(),
            rrf_fused_hits: Vec::new(),
            graph_expanded_hits: Vec::new(),
            reranker: crate::types::RetrievalRerankDebug::disabled(),
            final_evidence_pack: Vec::new(),
            display_evidence_pack: Vec::new(),
        }),
        context: None,
        collection_filter: None,
    };
    let encoded = serde_json::to_value(response).unwrap();

    assert_taxonomy_paths_resolve("ask retrieval debug", &encoded);
    assert!(encoded["text_taxonomy"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .any(|field| field["field"] == "retrieval.dense_vector_path"));
}

#[test]
fn taxonomy_omits_unselected_evidence_locator_variants() {
    let response: EvidenceResponse = serde_json::from_value(serde_json::json!({
        "id": "ev-1",
        "source_id": "src-1",
        "source_bounded": true,
        "text_hash": "hash",
        "kind": "text",
        "derived_from": null,
        "locator": "doc.md L1",
        "structured_locator": {
            "type": "Document",
            "path_or_url": "doc.md",
            "line_start": 1,
            "line_end": 1
        },
        "text": "quote",
        "heading_path": [],
        "language": null,
        "position": 0,
        "image_artifact": null
    }))
    .unwrap();
    let encoded = serde_json::to_value(response).unwrap();
    let fields = encoded["text_taxonomy"]["fields"].as_array().unwrap();

    assert!(!fields
        .iter()
        .any(|field| field["field"] == "structured_locator.ocr.profile.provider"));
    assert!(!fields
        .iter()
        .any(|field| field["field"] == "image_artifact.path"));
    assert_taxonomy_paths_resolve("document evidence", &encoded);
}

#[test]
fn legacy_ask_generated_citation_does_not_fall_into_evidence() {
    let response: AskResponse = serde_json::from_str(include_str!(
        "fixtures/legacy_ask_response_without_taxonomy.json"
    ))
    .unwrap();
    let field = response
        .text_taxonomy
        .fields
        .iter()
        .find(|field| field.field.ends_with("text_preview"))
        .expect("legacy citation classification");

    assert_ne!(field.plane, OutputTextPlane::Evidence);
}

#[test]
fn legacy_retrieve_caption_does_not_fall_into_evidence() {
    let response: RetrieveResponse = serde_json::from_str(include_str!(
        "fixtures/legacy_retrieve_caption_without_taxonomy.json"
    ))
    .unwrap();
    let field = response
        .text_taxonomy
        .fields
        .iter()
        .find(|field| field.field.ends_with("snippet"))
        .expect("legacy result classification");

    assert_ne!(field.plane, OutputTextPlane::Evidence);
}

#[test]
fn ocr_and_unknown_retrieval_roles_fail_closed_outside_evidence() {
    let citations = [CitationResponse {
        label: "E1".into(),
        evidence_id: "ev-ocr".into(),
        kind: "ocr".into(),
        derived_from: None,
        collections: Vec::new(),
        locator: "ocr".into(),
        text_preview: "OCR text".into(),
    }];
    let ask_taxonomy = ResponseTextTaxonomy::ask_response_with_citations(&citations);
    assert_ne!(
        ask_taxonomy
            .fields
            .iter()
            .find(|field| field.field.ends_with("text_preview"))
            .unwrap()
            .plane,
        OutputTextPlane::Evidence
    );

    let results = [RetrieveResultResponse {
        index: 0,
        rank: 1,
        label: "E1".into(),
        evidence_id: "ev-unknown".into(),
        text_hash: "hash".into(),
        source_id: "src".into(),
        source_hash: "source-hash".into(),
        source_path: None,
        collections: Vec::new(),
        chunk_id: "chunk".into(),
        kind: "text".into(),
        role: "unknown_role".into(),
        score: 1.0,
        locator: "unknown".into(),
        structured_locator: None,
        provenance: None,
        derived_from: None,
        snippet: "unknown text".into(),
    }];
    let retrieve_taxonomy = ResponseTextTaxonomy::retrieve_response_with_results(&results);
    assert_ne!(
        retrieve_taxonomy
            .fields
            .iter()
            .find(|field| field.field.ends_with("snippet"))
            .unwrap()
            .plane,
        OutputTextPlane::Evidence
    );
}
