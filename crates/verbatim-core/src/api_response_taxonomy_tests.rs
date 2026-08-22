use std::collections::HashSet;

use super::*;

#[test]
fn ask_response_taxonomy_never_labels_generated_or_interface_text_as_evidence() {
    let response = AskResponse {
        answer: "Legacy generated answer.".into(),
        answer_kind: AnswerKind::GeneratedInterpretation,
        text_taxonomy: ResponseTextTaxonomy::ask_response(),
        generated_interpretation: Some(GeneratedInterpretationResponse {
            text: "Generated interpretation.".into(),
        }),
        citations: vec![CitationResponse {
            label: "E1".into(),
            evidence_id: "ev-1".into(),
            kind: "original_text".into(),
            derived_from: None,
            collections: Vec::new(),
            locator: "doc.md L1".into(),
            text_preview: "Persisted source quote.".into(),
        }],
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
        plane_for("citations[].text_preview"),
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
            }],
            "locator": "doc.md L1",
            "text_preview": "quote"
        }],
        "verified": false,
        "collection_filter": {
            "requested": {
                "collection_ids": ["col-1"],
                "names": ["docs"],
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
            }],
            "warnings": ["none"],
            "stale": false
        }
    }))
    .unwrap();
    let retrieve: RetrieveResponse = serde_json::from_value(serde_json::json!({
        "task_id": "task-1",
        "query": "question",
        "source_id": "src-1",
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
            }],
            "chunk_id": "chunk-1",
            "kind": "text",
            "role": "original_text",
            "score": 1.0,
            "locator": "doc.md L1",
            "derived_from": "ev-0",
            "snippet": "quote"
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
        "image_artifact": null
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
        let matches = resolve_taxonomy_path(response, path);
        assert_eq!(matches.len(), 1, "{name} taxonomy path {path:?}");
        assert!(matches[0].is_string(), "{name} taxonomy path {path:?}");
    }
}

fn resolve_taxonomy_path<'a>(
    value: &'a serde_json::Value,
    path: &str,
) -> Vec<&'a serde_json::Value> {
    let Some((segment, rest)) = path.split_once('.') else {
        if let Some(key) = path.strip_suffix("[]") {
            return value
                .get(key)
                .and_then(serde_json::Value::as_array)
                .into_iter()
                .flatten()
                .collect();
        }
        return value.get(path).into_iter().collect();
    };
    if let Some(key) = segment.strip_suffix("[]") {
        return value
            .get(key)
            .and_then(serde_json::Value::as_array)
            .into_iter()
            .flatten()
            .flat_map(|item| resolve_taxonomy_path(item, rest))
            .collect();
    }
    value
        .get(segment)
        .into_iter()
        .flat_map(|item| resolve_taxonomy_path(item, rest))
        .collect()
}
