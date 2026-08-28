use super::SourceResponse;
use crate::types::{OcrSourceStatus, SourceIngestDiagnostics, SourceOcrDiagnostics};
use crate::wire_schemas::{
    encode_wire_document, wire_content_hash, CanonicalIdentity, WireArtifactKind, WireSchemaVersion,
};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Serialize)]
struct SourceResponseBody<'a> {
    id: &'a str,
    path: &'a str,
    status: &'a str,
    hash: &'a str,
    parser_used: Option<&'a str>,
    last_ingested_at: Option<&'a str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    diagnostics: Option<&'a SourceIngestDiagnostics>,
}

fn sample_source() -> SourceResponse {
    SourceResponse::new(
        "source-record-fixture",
        "/tmp/source-record.md",
        "Indexed",
        "source-hash",
        Some("plaintext".into()),
        Some("2026-08-28T12:00:00Z".into()),
        None,
    )
    .unwrap()
}

fn sample_diagnostics() -> SourceIngestDiagnostics {
    SourceIngestDiagnostics {
        pdf: None,
        ocr: SourceOcrDiagnostics {
            enabled: false,
            status: OcrSourceStatus::NotRequired,
            current_profile: None,
            current_profile_hash: None,
            evidence_count: 2,
            evidence_profile_hashes: Vec::new(),
        },
    }
}

fn body(source: &SourceResponse) -> SourceResponseBody<'_> {
    SourceResponseBody {
        id: &source.id,
        path: &source.path,
        status: &source.status,
        hash: &source.hash,
        parser_used: source.parser_used.as_deref(),
        last_ingested_at: source.last_ingested_at.as_deref(),
        diagnostics: source.diagnostics.as_ref(),
    }
}

fn valid_wire(source: &SourceResponse) -> Value {
    let body = body(source);
    let identity = CanonicalIdentity::from_body(
        WireArtifactKind::SourceRecord,
        WireSchemaVersion::new(1, 0, 0),
        &source.id,
        &encode_wire_document(&body).unwrap(),
    )
    .unwrap();
    let mut wire = serde_json::to_value(body).unwrap();
    wire["identity"] = serde_json::to_value(identity).unwrap();
    wire
}

#[test]
fn source_response_publishes_source_record_identity_for_exact_public_body() {
    let source = sample_source();
    let wire = serde_json::to_value(&source).expect("source record encodes");
    let body = body(&source);
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::SourceRecord,
        WireSchemaVersion::new(1, 0, 0),
        &source.id,
        &encode_wire_document(&body).unwrap(),
    )
    .unwrap();

    assert_eq!(wire["id"], source.id);
    assert_eq!(wire["identity"]["kind"], "source_record");
    assert_eq!(
        wire["identity"]["schema_version"],
        json!({"major": 1, "minor": 0, "patch": 0})
    );
    assert_eq!(wire["identity"]["artifact_id"], source.id);
    assert_eq!(
        wire["identity"]["content_hash"],
        expected.content_hash.as_str()
    );
}

#[test]
fn source_response_identity_includes_present_diagnostics_in_public_body() {
    let source = SourceResponse::new(
        "source-record-fixture",
        "/tmp/source-record.md",
        "Indexed",
        "source-hash",
        Some("plaintext".into()),
        Some("2026-08-28T12:00:00Z".into()),
        Some(sample_diagnostics()),
    )
    .unwrap();
    let wire = serde_json::to_value(&source).expect("diagnostic source record encodes");
    let body = body(&source);
    let expected_hash = wire_content_hash(&encode_wire_document(&body).unwrap());

    assert_eq!(
        wire["diagnostics"],
        serde_json::to_value(source.diagnostics).unwrap()
    );
    assert_eq!(wire["identity"]["content_hash"], expected_hash);
}

#[test]
fn source_response_rejects_independent_public_and_identity_mutations() {
    let source = sample_source();
    let mutations = [
        ("id", json!("different-source")),
        ("path", json!("/tmp/other.md")),
        ("status", json!("Pending")),
        ("hash", json!("different-hash")),
        ("parser_used", json!("pdf_oxide")),
        ("last_ingested_at", json!("2026-08-29T12:00:00Z")),
        (
            "diagnostics",
            json!({
                "pdf": null,
                "ocr": {
                    "enabled": true,
                    "status": "disabled",
                    "evidence_count": 0,
                    "evidence_profile_hashes": []
                }
            }),
        ),
        ("identity.kind", json!("source_created")),
        (
            "identity.schema_version",
            json!({"major": 9, "minor": 0, "patch": 0}),
        ),
        ("identity.artifact_id", json!("different-source")),
        ("identity.content_hash", json!("deadbeef")),
    ];

    for (path, value) in mutations {
        let mut wire = valid_wire(&source);
        let (field, nested_field) = path
            .split_once('.')
            .map_or((path, None), |(field, nested)| (field, Some(nested)));
        if let Some(nested_field) = nested_field {
            wire[field][nested_field] = value;
        } else {
            wire[field] = value;
        }
        assert!(
            serde_json::from_value::<SourceResponse>(wire).is_err(),
            "mutation {path} must fail closed"
        );
    }
}
