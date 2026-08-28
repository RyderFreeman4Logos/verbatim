use super::TaskCreatedResponse;
use crate::wire_schemas::{
    encode_wire_document, wire_content_hash, ContentHash, WireArtifactKind, WireSchemaVersion,
};
use serde_json::{json, Value};

fn valid_wire(task_id: &str) -> Value {
    let body = json!({"task_id": task_id});
    json!({
        "task_id": task_id,
        "identity": {
            "kind": "task_created",
            "schema_version": {"major": 1, "minor": 0, "patch": 0},
            "artifact_id": task_id,
            "content_hash": wire_content_hash(&encode_wire_document(&body).unwrap()),
        }
    })
}

#[test]
fn task_created_response_publishes_task_created_identity() {
    let response =
        TaskCreatedResponse::new("task-created-fixture").expect("task-created response constructs");
    let wire = serde_json::to_value(response).expect("task-created response encodes");

    assert_eq!(wire["task_id"], "task-created-fixture");
    assert_eq!(wire["identity"]["kind"], "task_created");
    assert_eq!(
        wire["identity"]["schema_version"],
        json!({"major": 1, "minor": 0, "patch": 0})
    );
    assert_eq!(wire["identity"]["artifact_id"], "task-created-fixture");
    assert!(wire["identity"]["content_hash"].is_string());
}

#[test]
fn task_created_response_decodes_ask_ingest_and_reindex_fixtures() {
    for task_id in ["ask-task", "ingest-task", "reindex-task"] {
        let response: TaskCreatedResponse =
            serde_json::from_value(valid_wire(task_id)).expect("task-created fixture decodes");
        assert_eq!(response.task_id, task_id);
    }
}

#[test]
fn task_created_response_rejects_mutated_identity_on_serialization() {
    for field in [
        "task_id",
        "identity.kind",
        "identity.schema_version",
        "identity.artifact_id",
        "identity.content_hash",
    ] {
        let mut response =
            TaskCreatedResponse::new("task-created-fixture").expect("task-created response");
        match field {
            "task_id" => response.task_id = "different-task".into(),
            "identity.kind" => response.identity.kind = WireArtifactKind::TaskRun,
            "identity.schema_version" => {
                response.identity.schema_version = WireSchemaVersion::new(9, 0, 0)
            }
            "identity.artifact_id" => response.identity.artifact_id = "different-task".into(),
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
fn task_created_response_rejects_independent_wire_mutations() {
    let mutations = [
        ("task_id", json!("different-task")),
        ("identity.kind", json!("task_run")),
        (
            "identity.schema_version",
            json!({"major": 9, "minor": 0, "patch": 0}),
        ),
        ("identity.artifact_id", json!("different-task")),
        ("identity.content_hash", json!("deadbeef")),
    ];

    for (path, value) in mutations {
        let mut wire = valid_wire("task-created-fixture");
        let (identity_field, nested_field) = path
            .split_once('.')
            .map_or((path, None), |(identity, field)| (identity, Some(field)));
        if let Some(nested_field) = nested_field {
            wire[identity_field][nested_field] = value;
        } else {
            wire[identity_field] = value;
        }
        assert!(
            serde_json::from_value::<TaskCreatedResponse>(wire).is_err(),
            "mutation {path} must fail closed"
        );
    }
}
