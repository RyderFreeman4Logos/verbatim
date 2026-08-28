use super::AddSourceResponse;
use crate::wire_schemas::{
    encode_wire_document, wire_content_hash, ContentHash, WireArtifactKind, WireSchemaVersion,
};
use serde_json::{json, Value};

fn valid_wire(id: &str) -> Value {
    let body = json!({"id": id});
    json!({
        "id": id,
        "identity": {
            "kind": "source_created",
            "schema_version": {"major": 1, "minor": 0, "patch": 0},
            "artifact_id": id,
            "content_hash": wire_content_hash(&encode_wire_document(&body).unwrap()),
        }
    })
}

#[test]
fn source_created_response_publishes_identity_for_public_id_body() {
    let id = "source-created-fixture";
    let response = AddSourceResponse::new(id).expect("source-created response constructs");
    let wire = serde_json::to_value(&response).expect("source-created response encodes");
    let body = json!({"id": id});

    assert_eq!(wire["id"], id);
    assert_eq!(wire["identity"]["kind"], "source_created");
    assert_eq!(
        wire["identity"]["schema_version"],
        json!({"major": 1, "minor": 0, "patch": 0})
    );
    assert_eq!(wire["identity"]["artifact_id"], id);
    assert_eq!(
        wire["identity"]["content_hash"],
        wire_content_hash(&encode_wire_document(&body).unwrap())
    );

    let decoded: AddSourceResponse =
        serde_json::from_value(valid_wire(id)).expect("source-created fixture decodes");
    assert_eq!(decoded, response);
}

#[test]
fn source_created_response_rejects_mutated_identity_on_serialization() {
    for field in [
        "id",
        "identity.kind",
        "identity.schema_version",
        "identity.artifact_id",
        "identity.content_hash",
    ] {
        let mut response = AddSourceResponse::new("source-created-fixture")
            .expect("source-created response constructs");
        match field {
            "id" => response.id = "different-source".into(),
            "identity.kind" => response.identity.kind = WireArtifactKind::TaskCreated,
            "identity.schema_version" => {
                response.identity.schema_version = WireSchemaVersion::new(9, 0, 0)
            }
            "identity.artifact_id" => response.identity.artifact_id = "different-source".into(),
            "identity.content_hash" => {
                response.identity.content_hash = ContentHash::new("deadbeef").unwrap()
            }
            _ => unreachable!(),
        }
        assert!(
            serde_json::to_value(response).is_err(),
            "mutation {field} must fail closed during serialization"
        );
    }
}

#[test]
fn source_created_response_rejects_independent_wire_mutations() {
    let mutations = [
        ("id", json!("different-source")),
        ("identity.kind", json!("task_created")),
        (
            "identity.schema_version",
            json!({"major": 9, "minor": 0, "patch": 0}),
        ),
        ("identity.artifact_id", json!("different-source")),
        ("identity.content_hash", json!("deadbeef")),
    ];

    for (path, value) in mutations {
        let mut wire = valid_wire("source-created-fixture");
        let (identity_field, nested_field) = path
            .split_once('.')
            .map_or((path, None), |(identity, field)| (identity, Some(field)));
        if let Some(nested_field) = nested_field {
            wire[identity_field][nested_field] = value;
        } else {
            wire[identity_field] = value;
        }
        assert!(
            serde_json::from_value::<AddSourceResponse>(wire).is_err(),
            "mutation {path} must fail closed"
        );
    }
}
