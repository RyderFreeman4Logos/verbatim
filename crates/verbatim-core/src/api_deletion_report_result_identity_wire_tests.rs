use crate::deletion::{DeletionReport, PersistedDeletionReport, RetentionPolicy};
use crate::types::SourceId;
use crate::wire_schemas::{encode_wire_document, wire_content_hash, WIRE_SCHEMA_VERSION};
use crate::DeletionReportResponse;
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Serialize)]
struct DeletionReportResponseBody<'a> {
    source_id: &'a SourceId,
    recorded_at: &'a str,
    retention_policy: RetentionPolicy,
    report: &'a DeletionReport,
}

fn receipt() -> PersistedDeletionReport {
    PersistedDeletionReport {
        source_id: SourceId("source-deletion-fixture".into()),
        recorded_at: "2026-08-30T12:00:00Z".into(),
        retention_policy: RetentionPolicy::until_backup_expiry(1_800_000_000),
        report: DeletionReport::new(),
    }
}

fn valid_wire(receipt: &PersistedDeletionReport) -> Value {
    let body = DeletionReportResponseBody {
        source_id: &receipt.source_id,
        recorded_at: &receipt.recorded_at,
        retention_policy: receipt.retention_policy,
        report: &receipt.report,
    };
    json!({
        "source_id": receipt.source_id,
        "recorded_at": receipt.recorded_at,
        "retention_policy": receipt.retention_policy,
        "report": receipt.report,
        "identity": {
            "kind": "deletion_report_result",
            "schema_version": WIRE_SCHEMA_VERSION,
            "artifact_id": receipt.source_id.0,
            "content_hash": wire_content_hash(&encode_wire_document(&body).unwrap()),
        }
    })
}

#[test]
fn deletion_report_result_identity_is_canonical_with_exact_body_and_public_order() {
    let receipt = receipt();
    let first = DeletionReportResponse::new(receipt.clone()).unwrap();
    let second = DeletionReportResponse::new(receipt.clone()).unwrap();
    assert_eq!(first.identity, second.identity);
    assert_eq!(first.identity.kind.as_str(), "deletion_report_result");
    assert_eq!(first.identity.schema_version, WIRE_SCHEMA_VERSION);
    assert_eq!(first.identity.artifact_id, receipt.source_id.0);

    let body = DeletionReportResponseBody {
        source_id: &receipt.source_id,
        recorded_at: &receipt.recorded_at,
        retention_policy: receipt.retention_policy,
        report: &receipt.report,
    };
    let canonical_body = String::from_utf8(encode_wire_document(&body).unwrap()).unwrap();
    assert!(canonical_body.starts_with("{\"source_id\":"));
    assert!(
        canonical_body.find("\"source_id\"").unwrap()
            < canonical_body.find("\"recorded_at\"").unwrap()
    );
    assert!(
        canonical_body.find("\"recorded_at\"").unwrap()
            < canonical_body.find("\"retention_policy\"").unwrap()
    );
    assert!(
        canonical_body.find("\"retention_policy\"").unwrap()
            < canonical_body.find("\"report\"").unwrap()
    );
    assert!(!canonical_body.contains("\"identity\""));
    assert_eq!(
        first.identity.content_hash.as_str(),
        wire_content_hash(canonical_body.as_bytes())
    );

    let encoded = serde_json::to_string(&first).unwrap();
    assert!(encoded.starts_with("{\"source_id\":"));
    assert!(encoded.find("\"source_id\"").unwrap() < encoded.find("\"recorded_at\"").unwrap());
    assert!(
        encoded.find("\"recorded_at\"").unwrap() < encoded.find("\"retention_policy\"").unwrap()
    );
    assert!(encoded.find("\"retention_policy\"").unwrap() < encoded.find("\"report\"").unwrap());
    assert!(encoded.find("\"report\"").unwrap() < encoded.find("\"identity\"").unwrap());

    let persisted_wire = serde_json::to_string(&receipt).unwrap();
    assert!(persisted_wire.starts_with("{\"source_id\":"));
    assert!(!persisted_wire.contains("\"identity\""));
    assert_eq!(
        serde_json::from_str::<PersistedDeletionReport>(&persisted_wire).unwrap(),
        receipt
    );
}

#[test]
fn deletion_report_result_identity_deserializes_valid_receipt_and_fails_closed() {
    let receipt = receipt();
    let wire = valid_wire(&receipt);
    let decoded: DeletionReportResponse = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(decoded.source_id, receipt.source_id);
    assert_eq!(decoded.recorded_at, receipt.recorded_at);
    assert_eq!(decoded.retention_policy, receipt.retention_policy);
    assert_eq!(decoded.report, receipt.report);

    for (path, value) in [
        ("identity.content_hash", json!("deadbeef")),
        ("identity.kind", json!("source_record")),
        ("identity.artifact_id", json!("different-source")),
        (
            "identity.schema_version",
            json!({"major": 9, "minor": 0, "patch": 0}),
        ),
    ] {
        let mut mutated = wire.clone();
        let (_, field) = path.split_once('.').unwrap();
        mutated["identity"][field] = value;
        assert!(
            serde_json::from_value::<DeletionReportResponse>(mutated).is_err(),
            "mutation {path} must fail closed"
        );
    }

    for field in ["source_id", "identity"] {
        let mut missing = wire.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<DeletionReportResponse>(missing).is_err(),
            "missing field {field} must fail closed"
        );
    }

    let mut unknown = wire.clone();
    unknown["unexpected"] = json!(true);
    assert!(serde_json::from_value::<DeletionReportResponse>(unknown).is_err());

    let mut unknown_identity = wire;
    unknown_identity["identity"]["unexpected"] = json!(true);
    assert!(serde_json::from_value::<DeletionReportResponse>(unknown_identity).is_err());
}
