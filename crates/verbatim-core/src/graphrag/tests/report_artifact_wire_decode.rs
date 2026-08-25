use crate::graphrag::ReportArtifactManifest;
use serde_json::{json, Value};

fn valid_manifest_json() -> Value {
    json!({
        "id": "graphrag://report/c1",
        "schema_version": {"major": 1, "minor": 0, "patch": 0},
        "derived_kind": "graph_report",
        "generation": "gen-1",
        "content_hash": "abc123",
        "report": {
            "id": "c1",
            "title": "t",
            "summary": "s",
            "claims": [],
            "evidence": []
        }
    })
}

fn decode_manifest(value: &Value) -> Result<ReportArtifactManifest, serde_json::Error> {
    serde_json::from_str(&value.to_string())
}

#[test]
fn report_artifact_manifest_decode_rejects_unknown_schema_version() {
    let mut wire = valid_manifest_json();
    wire["schema_version"] = json!({"major": 99, "minor": 0, "patch": 0});
    decode_manifest(&wire).expect_err("unknown schema_version must fail closed");
}

#[test]
fn report_artifact_manifest_decode_rejects_empty_generation() {
    let mut wire = valid_manifest_json();
    wire["generation"] = json!("");
    decode_manifest(&wire).expect_err("empty generation must fail closed");
}

#[test]
fn report_artifact_manifest_decode_rejects_empty_content_hash() {
    let mut wire = valid_manifest_json();
    wire["content_hash"] = json!("");
    decode_manifest(&wire).expect_err("empty content hash must fail closed");
}

#[test]
fn report_artifact_manifest_decode_rejects_whitespace_content_hash() {
    let mut wire = valid_manifest_json();
    wire["content_hash"] = json!("   ");
    decode_manifest(&wire).expect_err("whitespace content hash must fail closed");
}
