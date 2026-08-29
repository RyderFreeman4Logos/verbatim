use serde_json::{json, Value};

use crate::wire_schemas::{ContentHash, WireArtifactKind, WireSchemaVersion};

#[test]
fn reindex_result_identity() {
    for reindexed in [0, 2] {
        let response = ReindexResponse::new(reindexed).expect("reindex response constructs");
        let wire = serde_json::to_value(&response).expect("reindex response encodes");

        assert_eq!(wire["identity"]["kind"], "reindex_result");
        assert_eq!(wire["identity"]["artifact_id"], "reindex-result");
        assert_eq!(
            wire["identity"]["schema_version"],
            json!({"major": 1, "minor": 0, "patch": 0})
        );
        let decoded: ReindexResponse =
            serde_json::from_value(wire).expect("reindex response decodes");
        assert_eq!(decoded, response);
    }
}

#[test]
fn reindex_result_identity_requires_identity() {
    let error = serde_json::from_value::<ReindexResponse>(json!({"reindexed": 1}))
        .expect_err("reindex response without identity must fail closed");
    assert!(error.to_string().contains("missing field"), "{error}");
}

#[test]
fn reindex_result_identity_rejects_mutations() {
    let valid_response = ReindexResponse::new(2).expect("reindex response constructs");
    let valid_wire = serde_json::to_value(&valid_response).expect("reindex response encodes");

    for (label, mutated) in [
        (
            "reindexed",
            mutate(&valid_wire, |wire| wire["reindexed"] = json!(3)),
        ),
        (
            "kind",
            mutate(&valid_wire, |wire| {
                wire["identity"]["kind"] = json!("source_record")
            }),
        ),
        (
            "schema_version",
            mutate(&valid_wire, |wire| {
                wire["identity"]["schema_version"] = json!({"major": 2, "minor": 0, "patch": 0})
            }),
        ),
        (
            "artifact_id",
            mutate(&valid_wire, |wire| {
                wire["identity"]["artifact_id"] = json!("other")
            }),
        ),
        (
            "content_hash",
            mutate(&valid_wire, |wire| {
                wire["identity"]["content_hash"] =
                    json!("0000000000000000000000000000000000000000000000000000000000000000")
            }),
        ),
    ] {
        let error = serde_json::from_value::<ReindexResponse>(mutated)
            .expect_err("mutated reindex response must fail closed");
        assert!(!error.to_string().is_empty(), "{label}: {error}");
    }

    let mut mutated = valid_response.clone();
    mutated.reindexed = 3;
    assert!(
        serde_json::to_value(&mutated).is_err(),
        "reindexed mutation"
    );

    let mut mutated = valid_response.clone();
    mutated.identity.kind = WireArtifactKind::SourceRecord;
    assert!(serde_json::to_value(&mutated).is_err(), "kind mutation");

    let mut mutated = valid_response.clone();
    mutated.identity.schema_version = WireSchemaVersion::new(2, 0, 0);
    assert!(
        serde_json::to_value(&mutated).is_err(),
        "schema_version mutation"
    );

    let mut mutated = valid_response.clone();
    mutated.identity.artifact_id = "other".into();
    assert!(
        serde_json::to_value(&mutated).is_err(),
        "artifact_id mutation"
    );

    let mut mutated = valid_response;
    mutated.identity.content_hash =
        ContentHash::new("0000000000000000000000000000000000000000000000000000000000000000")
            .expect("content hash fixture");
    assert!(
        serde_json::to_value(&mutated).is_err(),
        "content_hash mutation"
    );
}

fn mutate(valid: &Value, mutation: impl FnOnce(&mut Value)) -> Value {
    let mut wire = valid.clone();
    mutation(&mut wire);
    wire
}
