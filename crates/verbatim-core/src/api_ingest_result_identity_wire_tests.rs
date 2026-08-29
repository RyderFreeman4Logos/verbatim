use serde_json::{json, Value};

use super::IngestResponse;

#[test]
fn ingest_result_identity() {
    for ingested in [0, 2] {
        let response = IngestResponse::new(ingested).expect("ingest response constructs");
        let wire = serde_json::to_value(&response).expect("ingest response encodes");

        assert_eq!(wire["identity"]["kind"], "ingest_result");
        assert_eq!(wire["identity"]["artifact_id"], "ingest-result");
        assert_eq!(
            wire["identity"]["schema_version"],
            json!({"major": 1, "minor": 0, "patch": 0})
        );
        let decoded: IngestResponse =
            serde_json::from_value(wire).expect("ingest response decodes");
        assert_eq!(decoded, response);
    }
}

#[test]
fn ingest_result_identity_requires_identity() {
    let error = serde_json::from_value::<IngestResponse>(json!({"ingested": 1}))
        .expect_err("ingest response without identity must fail closed");
    assert!(error.to_string().contains("missing field"), "{error}");
}

#[test]
fn ingest_result_identity_rejects_mutations() {
    let valid = json!({
        "ingested": 2,
        "identity": {
            "kind": "ingest_result",
            "schema_version": {"major": 1, "minor": 0, "patch": 0},
            "artifact_id": "ingest-result",
            "content_hash": "5b226208ed38de454400a0afa419efce6014960ef3c01723ae1468a5f71a48f8"
        }
    });

    for (label, mutated) in [
        (
            "ingested",
            mutate(&valid, |wire| wire["ingested"] = json!(3)),
        ),
        (
            "kind",
            mutate(&valid, |wire| {
                wire["identity"]["kind"] = json!("source_record")
            }),
        ),
        (
            "schema_version",
            mutate(&valid, |wire| {
                wire["identity"]["schema_version"] = json!({"major": 2, "minor": 0, "patch": 0})
            }),
        ),
        (
            "artifact_id",
            mutate(&valid, |wire| {
                wire["identity"]["artifact_id"] = json!("other")
            }),
        ),
        (
            "content_hash",
            mutate(&valid, |wire| {
                wire["identity"]["content_hash"] =
                    json!("0000000000000000000000000000000000000000000000000000000000000000")
            }),
        ),
    ] {
        let error = serde_json::from_value::<IngestResponse>(mutated)
            .expect_err("mutated ingest response must fail closed");
        assert!(!error.to_string().is_empty(), "{label}: {error}");
    }
}

fn mutate(valid: &Value, mutation: impl FnOnce(&mut Value)) -> Value {
    let mut wire = valid.clone();
    mutation(&mut wire);
    wire
}
