use serde_json::json;

use super::{TaskListAggregate, TaskListResponse};
use crate::task::{TaskId, TaskKind, TaskStatus, TaskSummary};
use crate::wire_schemas::{ContentHash, WireArtifactKind, WireSchemaVersion};

fn sample_task(id: &str) -> TaskSummary {
    TaskSummary {
        id: TaskId(id.into()),
        kind: TaskKind::Ask,
        status: TaskStatus::Running,
        created_at: "2026-08-28T12:00:00Z".into(),
        updated_at: "2026-08-28T12:01:00Z".into(),
        started_at: Some("2026-08-28T12:00:01Z".into()),
        finished_at: None,
        request: json!({"question": "public question", "api_key": "[REDACTED]"}),
        result: None,
        error: None,
        queue_position: Some(1),
        blocking_reason: None,
        progress: None,
    }
}

fn sample_aggregate() -> TaskListAggregate {
    serde_json::from_value(json!({
        "active_total": 1,
        "active_sample_size": 1,
        "active_sample_limit": 20,
        "turnover": {
            "window": {
                "event_sequence_floor": 1,
                "event_sequence_ceiling": 2,
                "event_limit": 100
            },
            "recent_terminalized": 1,
            "recent_succeeded": 1,
            "recent_failed": 0,
            "recent_cancelled": 0,
            "recent_backfilled": 0
        },
        "embedding_wait": {
            "waiting": 0,
            "oldest_wait_ms": null,
            "reason_buckets": []
        },
        "stale_running": {
            "publish_complete_running": 0,
            "reason_buckets": []
        }
    }))
    .unwrap()
}

fn response(
    tasks: Vec<TaskSummary>,
    total: usize,
    aggregate: Option<TaskListAggregate>,
) -> TaskListResponse {
    TaskListResponse::new(tasks, total, aggregate).unwrap()
}

#[test]
fn task_list_identity_covers_populated_and_empty_pages() {
    for response in [
        response(Vec::new(), 0, None),
        response(
            vec![sample_task("task-list-fixture")],
            1,
            Some(sample_aggregate()),
        ),
    ] {
        let encoded = serde_json::to_value(&response).unwrap();
        assert_eq!(encoded["identity"]["kind"], "task_list");
        assert_eq!(
            encoded["identity"]["schema_version"],
            json!({"major": 1, "minor": 0, "patch": 0})
        );
        assert_eq!(encoded["identity"]["artifact_id"], "task-list");
        assert!(encoded["identity"]["content_hash"].is_string());
        let decoded: TaskListResponse = serde_json::from_value(encoded).unwrap();
        assert_eq!(decoded, response);
    }
}

#[test]
fn task_list_identity_rejects_mutated_public_body_on_serialize() {
    let mut task = response(vec![sample_task("task-list-fixture")], 1, None);
    task.tasks[0].request["question"] = json!("mutated");
    assert!(serde_json::to_value(task).is_err());

    let mut order = response(
        vec![
            sample_task("task-list-first"),
            sample_task("task-list-second"),
        ],
        2,
        None,
    );
    order.tasks.reverse();
    assert!(serde_json::to_value(order).is_err());

    let mut total = response(vec![sample_task("task-list-fixture")], 1, None);
    total.total = 2;
    assert!(serde_json::to_value(total).is_err());

    let mut aggregate = response(
        vec![sample_task("task-list-fixture")],
        1,
        Some(sample_aggregate()),
    );
    aggregate.aggregate.as_mut().unwrap().active_total = 2;
    assert!(serde_json::to_value(aggregate).is_err());
}

#[test]
fn task_list_identity_rejects_each_mutated_identity_component_on_decode() {
    let encoded = serde_json::to_value(response(Vec::new(), 0, None)).unwrap();
    for (field, replacement) in [
        ("kind", json!("task_events")),
        (
            "schema_version",
            json!({"major": 9, "minor": 0, "patch": 0}),
        ),
        ("artifact_id", json!("task-list-other")),
        ("content_hash", json!("not-the-body-hash")),
    ] {
        let mut mutated = encoded.clone();
        mutated["identity"][field] = replacement;
        assert!(
            serde_json::from_value::<TaskListResponse>(mutated).is_err(),
            "mutating identity.{field} must fail closed"
        );
    }
}

#[test]
fn task_list_identity_rejects_each_mutated_identity_component_on_serialize() {
    let mut kind = response(Vec::new(), 0, None);
    kind.identity.kind = WireArtifactKind::TaskEvents;
    assert!(serde_json::to_value(kind).is_err());

    let mut schema_version = response(Vec::new(), 0, None);
    schema_version.identity.schema_version = WireSchemaVersion::new(9, 0, 0);
    assert!(serde_json::to_value(schema_version).is_err());

    let mut artifact_id = response(Vec::new(), 0, None);
    artifact_id.identity.artifact_id = "task-list-other".into();
    assert!(serde_json::to_value(artifact_id).is_err());

    let mut content_hash = response(Vec::new(), 0, None);
    content_hash.identity.content_hash = ContentHash::new("not-the-body-hash").unwrap();
    assert!(serde_json::to_value(content_hash).is_err());
}

#[test]
fn task_list_identity_wire_requires_identity() {
    let encoded = serde_json::to_value(response(Vec::new(), 0, None)).unwrap();
    let mut missing = encoded;
    missing.as_object_mut().unwrap().remove("identity");
    assert!(serde_json::from_value::<TaskListResponse>(missing).is_err());
}
