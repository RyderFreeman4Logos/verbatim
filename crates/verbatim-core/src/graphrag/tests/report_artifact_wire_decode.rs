use crate::graphrag::{CommunityReport, ReportArtifactManifest};
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

fn consistent_manifest_json() -> Value {
    let report = CommunityReport {
        id: "c1".into(),
        title: "t".into(),
        summary: "s".into(),
        claims: Vec::new(),
        evidence: Vec::new(),
        content_hash: String::new(),
        generation: "gen-1".into(),
    };
    let content_hash = report.recompute_content_hash().unwrap();
    json!({
        "id": "graphrag://report/c1",
        "schema_version": {"major": 1, "minor": 0, "patch": 0},
        "derived_kind": "graph_report",
        "generation": "gen-1",
        "content_hash": content_hash,
        "report": {
            "id": "c1",
            "title": "t",
            "summary": "s",
            "claims": [],
            "evidence": [],
            "generation": "gen-1",
            "content_hash": content_hash
        }
    })
}

#[test]
fn report_artifact_manifest_decode_rejects_outer_identity_disagreeing_with_report() {
    decode_manifest(&valid_manifest_json())
        .expect_err("outer generation/hash must match embedded report");

    let mut wrong_id = consistent_manifest_json();
    wrong_id["report"]["id"] = json!("other");
    decode_manifest(&wrong_id).expect_err("outer id must match embedded report");

    let mut wrong_generation = consistent_manifest_json();
    wrong_generation["report"]["generation"] = json!("gen-other");
    decode_manifest(&wrong_generation).expect_err("outer generation must match embedded report");
}

#[test]
fn report_artifact_manifest_decode_rejects_stale_content_hash() {
    let mut wire = consistent_manifest_json();
    wire["content_hash"] = json!("deadbeef");
    wire["report"]["content_hash"] = json!("deadbeef");
    decode_manifest(&wire).expect_err("content hash must match recompute_content_hash");
}

#[test]
fn report_artifact_manifest_consistent_identity_decodes() {
    decode_manifest(&consistent_manifest_json()).expect("matching outer and report identity");
}
