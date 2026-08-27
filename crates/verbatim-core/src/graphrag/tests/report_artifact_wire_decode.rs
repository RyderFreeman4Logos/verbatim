use crate::graphrag::{CommunityReport, ReportArtifactManifest};
use serde_json::{json, Value};

fn valid_manifest_json() -> Value {
    consistent_manifest_json()
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
    let error = decode_manifest(&wire).expect_err("empty generation must fail closed");
    assert!(
        error.to_string().contains("generation must not be empty"),
        "empty generation must reach its invariant: {error}"
    );
}

#[test]
fn report_artifact_manifest_decode_rejects_empty_content_hash() {
    let mut wire = valid_manifest_json();
    wire["content_hash"] = json!("");
    let error = decode_manifest(&wire).expect_err("empty content hash must fail closed");
    assert!(
        error
            .to_string()
            .contains("content hash does not match embedded report"),
        "empty content hash must reach its invariant: {error}"
    );
}

#[test]
fn report_artifact_manifest_decode_rejects_whitespace_content_hash() {
    let mut wire = valid_manifest_json();
    wire["content_hash"] = json!("   ");
    let error = decode_manifest(&wire).expect_err("whitespace content hash must fail closed");
    assert!(
        error
            .to_string()
            .contains("content hash does not match embedded report"),
        "whitespace content hash must reach its invariant: {error}"
    );
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
        "identity": manifest_identity(&content_hash),
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

fn manifest_identity(content_hash: &str) -> Value {
    json!({
        "kind": "derived_artifact",
        "schema_version": {"major": 1, "minor": 0, "patch": 0},
        "artifact_id": "graphrag://report/c1",
        "content_hash": content_hash
    })
}

#[test]
fn report_artifact_manifest_decode_rejects_outer_identity_disagreeing_with_report() {
    let mut wrong_id = valid_manifest_json();
    wrong_id["id"] = json!("graphrag://report/other");
    wrong_id["identity"]["artifact_id"] = json!("graphrag://report/other");
    let error = decode_manifest(&wrong_id).expect_err("outer id must match embedded report");
    assert!(
        error
            .to_string()
            .contains("artifact id does not match embedded report"),
        "outer id must reach its invariant: {error}"
    );

    let mut wrong_generation = valid_manifest_json();
    wrong_generation["generation"] = json!("gen-other");
    let error = decode_manifest(&wrong_generation)
        .expect_err("outer generation must match embedded report");
    assert!(
        error
            .to_string()
            .contains("generation does not match embedded report"),
        "outer generation must reach its invariant: {error}"
    );
}

#[test]
fn report_artifact_manifest_decode_rejects_stale_content_hash() {
    let mut wire = valid_manifest_json();
    wire["content_hash"] = json!("deadbeef");
    wire["report"]["content_hash"] = json!("deadbeef");
    let error = decode_manifest(&wire).expect_err("content hash must match recompute_content_hash");
    assert!(
        error
            .to_string()
            .contains("content hash does not match recomputed report hash"),
        "stale content hash must reach its invariant: {error}"
    );
}

#[test]
fn report_artifact_manifest_consistent_identity_decodes() {
    decode_manifest(&consistent_manifest_json()).expect("matching outer and report identity");
}

#[test]
fn report_artifact_manifest_decode_requires_matching_canonical_identity() {
    let mut missing = consistent_manifest_json();
    missing.as_object_mut().unwrap().remove("identity");
    decode_manifest(&missing).expect_err("manifest identity must be supplied");

    for (field, replacement) in [
        ("kind", json!("evidence")),
        (
            "schema_version",
            json!({"major": 1, "minor": 0, "patch": 1}),
        ),
        ("artifact_id", json!("graphrag://report/other")),
        ("content_hash", json!("deadbeef")),
    ] {
        let mut mismatched = consistent_manifest_json();
        mismatched["identity"][field] = replacement;
        decode_manifest(&mismatched)
            .expect_err("manifest identity {field} must match the reconstructed manifest");
    }
}
