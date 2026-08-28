use serde_json::json;

use super::TaskEventsResponse;
use crate::task::{TaskEvent, TaskId};
use crate::wire_schemas::{ContentHash, WireArtifactKind, WireSchemaVersion};

fn sample_event() -> TaskEvent {
    TaskEvent {
        sequence: 7,
        task_id: TaskId("task-events-fixture".into()),
        event_type: "progress".into(),
        message: "public message".into(),
        payload: json!({"visible": true}),
        created_at: "2026-08-28T12:00:00Z".into(),
    }
}

fn response(events: Vec<TaskEvent>) -> TaskEventsResponse {
    TaskEventsResponse::new(TaskId("task-events-fixture".into()), events).unwrap()
}

#[test]
fn task_events_identity_covers_non_empty_and_empty_pages() {
    for events in [vec![sample_event()], Vec::new()] {
        let response = response(events);
        let encoded = serde_json::to_value(&response).unwrap();
        assert_eq!(encoded["task_id"], "task-events-fixture");
        assert_eq!(encoded["identity"]["kind"], "task_events");
        assert_eq!(
            encoded["identity"]["schema_version"],
            json!({"major": 1, "minor": 0, "patch": 0})
        );
        assert_eq!(encoded["identity"]["artifact_id"], "task-events-fixture");
        let decoded: TaskEventsResponse = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, response);
    }
}

#[test]
fn task_events_identity_rejects_mutated_public_body_on_serialize() {
    let mut task_id = response(vec![sample_event()]);
    task_id.task_id = TaskId("task-events-other".into());
    assert!(serde_json::to_value(task_id).is_err());

    let mut event = response(vec![sample_event()]);
    event.events[0].message = "mutated message".into();
    assert!(serde_json::to_value(event).is_err());
}

#[test]
fn task_events_identity_rejects_each_mutated_identity_component_on_decode() {
    let encoded = serde_json::to_value(response(vec![sample_event()])).unwrap();
    for (field, replacement) in [
        ("kind", json!("task_profile")),
        (
            "schema_version",
            json!({"major": 9, "minor": 0, "patch": 0}),
        ),
        ("artifact_id", json!("task-events-other")),
        ("content_hash", json!("not-the-body-hash")),
    ] {
        let mut mutated = encoded.clone();
        mutated["identity"][field] = replacement;
        assert!(
            serde_json::from_value::<TaskEventsResponse>(mutated).is_err(),
            "mutating identity.{field} must fail closed"
        );
    }
}

#[test]
fn task_events_identity_rejects_each_mutated_identity_component_on_serialize() {
    let mut kind = response(vec![sample_event()]);
    kind.identity.kind = WireArtifactKind::TaskProfile;
    assert!(serde_json::to_value(kind).is_err());

    let mut schema_version = response(vec![sample_event()]);
    schema_version.identity.schema_version = WireSchemaVersion::new(9, 0, 0);
    assert!(serde_json::to_value(schema_version).is_err());

    let mut artifact_id = response(vec![sample_event()]);
    artifact_id.identity.artifact_id = "task-events-other".into();
    assert!(serde_json::to_value(artifact_id).is_err());

    let mut content_hash = response(vec![sample_event()]);
    content_hash.identity.content_hash = ContentHash::new("not-the-body-hash").unwrap();
    assert!(serde_json::to_value(content_hash).is_err());
}

#[test]
fn task_events_identity_wire_requires_identity_and_task_id() {
    let encoded = serde_json::to_value(response(Vec::new())).unwrap();
    for field in ["task_id", "identity"] {
        let mut missing = encoded.clone();
        missing.as_object_mut().unwrap().remove(field);
        assert!(
            serde_json::from_value::<TaskEventsResponse>(missing).is_err(),
            "missing {field} must fail closed"
        );
    }
}
