use super::AskRetrievalDebugEvent;
use crate::types::{
    EvidenceId, EvidenceKind, RetrievalDebug, RetrievalDebugEvidencePackMode,
    RetrievalDenseVectorPath, RetrievalEvidencePackEntry, RetrievalEvidenceRole,
    RetrievalLocalSpansMs, RetrievalLocatorDebug, RetrievalProvenance, RetrievalRerankDebug,
    SourceId, SourceLocator,
};
use crate::wire_schemas::{WireArtifactKind, WireSchemaVersion, WIRE_SCHEMA_VERSION};

fn sample_pack() -> RetrievalEvidencePackEntry {
    RetrievalEvidencePackEntry {
        label: "E1".into(),
        result_rank: 1,
        chunk_id: crate::types::ChunkId("chunk-1".into()),
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
            crate::types::ChunkId("chunk-1".into()),
            SourceId("src-1".into()),
        ),
    }
}

fn sample_debug() -> RetrievalDebug {
    RetrievalDebug {
        dense_vector_path: RetrievalDenseVectorPath::ResidentHnsw,
        query_embedding_latency_ms: Some(13),
        retrieval_search_sql_statement_count: Some(14),
        retrieval_resource_counters: Some(Default::default()),
        local_spans_ms: RetrievalLocalSpansMs {
            setup_ms: 1,
            query_embedding_ms: 2,
            dense_vector_search_ms: 3,
            vector_queue_wait_ms: None,
            vector_service_ms: None,
            bm25_search_ms: 4,
            rrf_fusion_ms: 5,
            debug_candidate_pack_ms: 6,
            rerank_total_ms: 7,
            result_hydration_ms: 8,
            graph_expansion_ms: 9,
            final_evidence_pack_ms: 10,
            display_evidence_pack_ms: 11,
            response_formatting_ms: 12,
            canonical_support_embedding_ms: None,
            canonical_display_selection_ms: None,
        },
        candidate_counters: Default::default(),
        evidence_pack_mode: RetrievalDebugEvidencePackMode::Full,
        final_evidence_count: 0,
        display_evidence_count: 0,
        bm25_hits: Vec::new(),
        dense_hits: Vec::new(),
        rrf_fused_hits: Vec::new(),
        graph_expanded_hits: Vec::new(),
        reranker: RetrievalRerankDebug::disabled(),
        final_evidence_pack: vec![sample_pack()],
        display_evidence_pack: vec![sample_pack()],
    }
}

fn retrieval_event_wire() -> serde_json::Value {
    serde_json::to_value(AskRetrievalDebugEvent::new(sample_debug()).unwrap()).unwrap()
}

#[test]
fn ask_retrieval_debug_event_serializes_bound_identity_in_public_order() {
    let encoded =
        serde_json::to_string(&AskRetrievalDebugEvent::new(sample_debug()).unwrap()).unwrap();
    let expected_order = [
        "dense_vector_path",
        "query_embedding_latency_ms",
        "retrieval_search_sql_statement_count",
        "retrieval_resource_counters",
        "local_spans_ms",
        "candidate_counters",
        "evidence_pack_mode",
        "final_evidence_count",
        "display_evidence_count",
        "bm25_hits",
        "dense_hits",
        "rrf_fused_hits",
        "graph_expanded_hits",
        "reranker",
        "final_evidence_pack",
        "display_evidence_pack",
        "identity",
    ];
    let mut start = 0;
    for field in expected_order {
        let marker = format!("\"{field}\":");
        let position = encoded[start..]
            .find(&marker)
            .unwrap_or_else(|| panic!("missing {field} in {encoded}"));
        start += position + marker.len();
    }

    let wire = retrieval_event_wire();
    let decoded: AskRetrievalDebugEvent = serde_json::from_value(wire).unwrap();
    assert_eq!(decoded.into_retrieval_debug(), sample_debug());
}

fn mutate_dense_vector_path(wire: &mut serde_json::Value) {
    wire["dense_vector_path"] = serde_json::json!("bm25_only");
}
fn mutate_query_embedding_latency(wire: &mut serde_json::Value) {
    wire["query_embedding_latency_ms"] = serde_json::json!(1);
}
fn mutate_sql_count(wire: &mut serde_json::Value) {
    wire["retrieval_search_sql_statement_count"] = serde_json::json!(1);
}
fn mutate_resources(wire: &mut serde_json::Value) {
    wire["retrieval_resource_counters"] = serde_json::json!({"major_page_faults": 1});
}
fn mutate_local_spans(wire: &mut serde_json::Value) {
    wire["local_spans_ms"]["setup_ms"] = serde_json::json!(99);
}
fn mutate_candidate_counters(wire: &mut serde_json::Value) {
    wire["candidate_counters"]["visited"] = serde_json::json!(1);
}
fn mutate_evidence_pack_mode(wire: &mut serde_json::Value) {
    wire["evidence_pack_mode"] = serde_json::json!("compact");
}
fn mutate_final_count(wire: &mut serde_json::Value) {
    wire["final_evidence_count"] = serde_json::json!(1);
}
fn mutate_display_count(wire: &mut serde_json::Value) {
    wire["display_evidence_count"] = serde_json::json!(1);
}
fn mutate_bm25_hits(wire: &mut serde_json::Value) {
    wire["bm25_hits"] = serde_json::json!(null);
}
fn mutate_dense_hits(wire: &mut serde_json::Value) {
    wire["dense_hits"] = serde_json::json!(null);
}
fn mutate_rrf_hits(wire: &mut serde_json::Value) {
    wire["rrf_fused_hits"] = serde_json::json!(null);
}
fn mutate_graph_hits(wire: &mut serde_json::Value) {
    wire["graph_expanded_hits"] = serde_json::json!(null);
}
fn mutate_reranker(wire: &mut serde_json::Value) {
    wire["reranker"]["status"] = serde_json::json!("succeeded");
}
fn mutate_final_pack(wire: &mut serde_json::Value) {
    wire["final_evidence_pack"] = serde_json::json!(null);
}
fn mutate_display_pack(wire: &mut serde_json::Value) {
    wire["display_evidence_pack"] = serde_json::json!(null);
}
fn mutate_identity(wire: &mut serde_json::Value) {
    wire["identity"]["content_hash"] = serde_json::json!("deadbeef");
}

#[test]
fn ask_retrieval_debug_event_rejects_every_canonical_field_mutation() {
    for (name, mutation) in [
        (
            "dense_vector_path",
            mutate_dense_vector_path as fn(&mut serde_json::Value),
        ),
        ("query_embedding_latency_ms", mutate_query_embedding_latency),
        ("retrieval_search_sql_statement_count", mutate_sql_count),
        ("retrieval_resource_counters", mutate_resources),
        ("local_spans_ms", mutate_local_spans),
        ("candidate_counters", mutate_candidate_counters),
        ("evidence_pack_mode", mutate_evidence_pack_mode),
        ("final_evidence_count", mutate_final_count),
        ("display_evidence_count", mutate_display_count),
        ("bm25_hits", mutate_bm25_hits),
        ("dense_hits", mutate_dense_hits),
        ("rrf_fused_hits", mutate_rrf_hits),
        ("graph_expanded_hits", mutate_graph_hits),
        ("reranker", mutate_reranker),
        ("final_evidence_pack", mutate_final_pack),
        ("display_evidence_pack", mutate_display_pack),
    ] {
        let mut wire = retrieval_event_wire();
        mutation(&mut wire);
        assert!(
            serde_json::from_value::<AskRetrievalDebugEvent>(wire).is_err(),
            "mutation of {name} must fail closed"
        );
    }
}

fn mutate_kind(wire: &mut serde_json::Value) {
    wire["identity"]["kind"] = serde_json::json!("derived_artifact");
}
fn mutate_schema_version(wire: &mut serde_json::Value) {
    wire["identity"]["schema_version"] = serde_json::json!({"major": 2, "minor": 0, "patch": 0});
}
fn mutate_artifact_id(wire: &mut serde_json::Value) {
    wire["identity"]["artifact_id"] = serde_json::json!("other-retrieval-debug");
}

#[test]
fn ask_retrieval_debug_event_rejects_identity_header_mutations() {
    for (name, mutation) in [
        ("kind", mutate_kind as fn(&mut serde_json::Value)),
        ("schema_version", mutate_schema_version),
        ("artifact_id", mutate_artifact_id),
        ("content_hash", mutate_identity),
    ] {
        let mut wire = retrieval_event_wire();
        mutation(&mut wire);
        assert!(
            serde_json::from_value::<AskRetrievalDebugEvent>(wire).is_err(),
            "mutation of identity {name} must fail closed"
        );
    }
}

#[test]
fn ask_retrieval_debug_event_uses_required_identity_metadata() {
    let wire = retrieval_event_wire();
    assert_eq!(wire["identity"]["kind"], "ask_retrieval_debug_event");
    assert_eq!(
        wire["identity"]["artifact_id"],
        "ask-stream-retrieval-debug"
    );
    assert_eq!(
        serde_json::from_value::<WireSchemaVersion>(wire["identity"]["schema_version"].clone())
            .unwrap(),
        WIRE_SCHEMA_VERSION
    );
    assert_eq!(
        serde_json::from_value::<WireArtifactKind>(wire["identity"]["kind"].clone()).unwrap(),
        WireArtifactKind::AskRetrievalDebugEvent
    );
}
