use crate::types::{RetrievalOrigin, RetrievalProvenance, RetrievalResult};
use serde_json::{json, Value};

fn valid_identity_provenance() -> Value {
    json!({
        "origin": "graph_report",
        "report_artifact_id": "graphrag://report/c1",
        "report_artifact_schema_version": {"major": 1, "minor": 0, "patch": 0},
        "report_artifact_generation": "gen-1",
        "report_artifact_content_hash": "abc123"
    })
}

fn decode_provenance(value: &Value) -> Result<RetrievalProvenance, serde_json::Error> {
    serde_json::from_str(&value.to_string())
}

fn enclosing_result(provenance: Value) -> Value {
    json!({
        "chunk_id": "chunk-1",
        "score": 1.0,
        "chunk": {
            "id": "chunk-1",
            "source_id": "src",
            "chunk_hash": "h",
            "text": "t",
            "context_text": null,
            "token_count": 1,
            "chunk_type": "Child",
            "parent_chunk_id": null,
            "heading_path": [],
            "evidence_unit_ids": []
        },
        "evidence_units": [],
        "provenance": provenance
    })
}

#[test]
fn retrieval_provenance_decode_rejects_unknown_schema_version() {
    let mut wire = valid_identity_provenance();
    wire["report_artifact_schema_version"] = json!({"major": 99, "minor": 0, "patch": 0});
    decode_provenance(&wire).expect_err("unknown schema_version must fail closed");
}

#[test]
fn retrieval_provenance_decode_rejects_empty_generation() {
    let mut wire = valid_identity_provenance();
    wire["report_artifact_generation"] = json!("");
    decode_provenance(&wire).expect_err("empty generation must fail closed");
}

#[test]
fn retrieval_provenance_decode_rejects_empty_content_hash() {
    let mut wire = valid_identity_provenance();
    wire["report_artifact_content_hash"] = json!("");
    decode_provenance(&wire).expect_err("empty content hash must fail closed");
}

#[test]
fn retrieval_provenance_decode_rejects_partial_identity() {
    let mut missing_generation = valid_identity_provenance();
    missing_generation
        .as_object_mut()
        .expect("object")
        .remove("report_artifact_generation");
    decode_provenance(&missing_generation).expect_err("missing generation must fail closed");

    let mut missing_hash = valid_identity_provenance();
    missing_hash
        .as_object_mut()
        .expect("object")
        .remove("report_artifact_content_hash");
    decode_provenance(&missing_hash).expect_err("missing hash must fail closed");

    let mut missing_id = valid_identity_provenance();
    missing_id
        .as_object_mut()
        .expect("object")
        .remove("report_artifact_id");
    decode_provenance(&missing_id).expect_err("missing id must fail closed");
}

#[test]
fn retrieval_result_decode_rejects_invalid_provenance_leaf() {
    let mut provenance = valid_identity_provenance();
    provenance["report_artifact_schema_version"] = json!({"major": 99, "minor": 0, "patch": 0});
    serde_json::from_str::<RetrievalResult>(&enclosing_result(provenance).to_string())
        .expect_err("enclosing RetrievalResult must reject invalid provenance");
}

#[test]
fn retrieval_provenance_graph_report_missing_identity_fails() {
    decode_provenance(&json!({"origin": "graph_report"}))
        .expect_err("GraphReport must carry complete report identity");
}

#[test]
fn retrieval_provenance_seed_must_not_carry_report_identity() {
    let mut wire = valid_identity_provenance();
    wire["origin"] = json!("seed");
    decode_provenance(&wire).expect_err("Seed must not carry report identity");
}

#[test]
fn retrieval_provenance_graph_expansion_must_not_carry_report_identity() {
    let mut wire = valid_identity_provenance();
    wire["origin"] = json!("graph_expansion");
    decode_provenance(&wire).expect_err("GraphExpansion must not carry report identity");
}

#[test]
fn retrieval_provenance_graph_report_round_trip_decodes() {
    let decoded = decode_provenance(&valid_identity_provenance()).expect("valid GraphReport");
    assert_eq!(decoded.origin, RetrievalOrigin::GraphReport);
    let encoded = serde_json::to_string(&decoded).expect("serialize");
    let round_trip: RetrievalProvenance = serde_json::from_str(&encoded).expect("round-trip");
    assert_eq!(decoded, round_trip);
}
