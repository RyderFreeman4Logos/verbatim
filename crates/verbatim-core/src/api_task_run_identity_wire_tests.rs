use super::{TaskMutationResponse, TaskSummaryResponse};
use crate::task::{TaskId, TaskKind, TaskSpan};
use crate::wire_schemas::{encode_wire_document, wire_content_hash};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Serialize)]
struct TaskSummaryIdentityBody<'a> {
    task: &'a crate::task::TaskSummary,
    spans: &'a [TaskSpan],
}

fn valid_task_summary_response(status: &str, with_span: bool) -> Value {
    let task = crate::task::TaskSummary {
        id: TaskId("task-fixture".into()),
        kind: TaskKind::Ask,
        status: serde_json::from_value(json!(status)).expect("task status fixture"),
        created_at: "1".into(),
        updated_at: "2".into(),
        started_at: Some("1".into()),
        finished_at: Some("2".into()),
        request: json!({"question_chars": 14}),
        result: Some(json!({"citation_count": 1})),
        error: None,
        queue_position: None,
        blocking_reason: None,
        progress: None,
    };
    let spans = if with_span {
        vec![TaskSpan {
            sequence: 1,
            task_id: TaskId("task-fixture".into()),
            phase: "retrieval".into(),
            started_at: "1".into(),
            duration_ms: 7,
            metadata: json!({"result_count": 1}),
        }]
    } else {
        Vec::new()
    };
    let body = TaskSummaryIdentityBody {
        task: &task,
        spans: &spans,
    };
    let body_bytes = encode_wire_document(&body).expect("body fixture encodes");
    json!({
        "task": serde_json::to_value(&task).expect("task fixture encodes"),
        "spans": serde_json::to_value(&spans).expect("span fixture encodes"),
        "identity": {
            "kind": "task_run",
            "schema_version": {"major": 1, "minor": 0, "patch": 0},
            "artifact_id": "task-fixture",
            "content_hash": wire_content_hash(&body_bytes)
        }
    })
}

fn decode(value: Value) -> Result<TaskSummaryResponse, serde_json::Error> {
    serde_json::from_value(value)
}

fn legacy_task_mutation_response(status: &str, with_span: bool) -> Value {
    let mut wire = valid_task_summary_response(status, with_span);
    wire.as_object_mut()
        .expect("task mutation fixture is an object")
        .remove("identity");
    wire
}

fn mutate_task_status(wire: &mut Value) {
    wire["task"]["status"] = json!("failed");
}

fn mutate_task_id(wire: &mut Value) {
    wire["task"]["id"] = json!("task-other");
}

fn mutate_span_metadata(wire: &mut Value) {
    wire["spans"][0]["metadata"]["result_count"] = json!(2);
}

fn mutate_content_hash(wire: &mut Value) {
    wire["identity"]["content_hash"] = json!("not-the-body-hash");
}

#[test]
fn task_summary_response_stamps_task_run_identity_for_lifecycle_snapshots() {
    for status in ["queued", "running", "succeeded", "failed", "cancelled"] {
        for with_span in [false, true] {
            let response = decode(valid_task_summary_response(status, with_span))
                .expect("valid task summary identity fixture decodes");
            let wire = serde_json::to_value(response).expect("task summary encodes");
            assert_eq!(wire["identity"]["kind"], "task_run", "status={status}");
            assert_eq!(wire["identity"]["artifact_id"], "task-fixture");
            assert_eq!(
                wire["identity"]["schema_version"],
                json!({
                    "major": 1,
                    "minor": 0,
                    "patch": 0
                })
            );
            assert!(
                !wire["identity"]["content_hash"]
                    .as_str()
                    .unwrap_or_default()
                    .is_empty(),
                "status={status} with_span={with_span}"
            );
        }
    }
}

#[test]
fn task_summary_response_rejects_identity_body_mismatch() {
    for (name, mutate) in [
        ("task_status", mutate_task_status as fn(&mut Value)),
        ("task_id", mutate_task_id),
        ("span_metadata", mutate_span_metadata),
        ("content_hash", mutate_content_hash),
    ] {
        let mut wire = valid_task_summary_response("succeeded", true);
        mutate(&mut wire);
        assert!(
            decode(wire).is_err(),
            "task summary identity mismatch must reject: {name}"
        );
    }
}

#[test]
fn task_mutation_response_keeps_legacy_wire_without_task_run_identity() {
    for status in ["queued", "running", "succeeded", "failed", "cancelled"] {
        for with_span in [false, true] {
            let wire = legacy_task_mutation_response(status, with_span);
            let response: TaskMutationResponse =
                serde_json::from_value(wire.clone()).expect("legacy task mutation decodes");
            let encoded = serde_json::to_value(response).expect("legacy task mutation encodes");
            assert_eq!(encoded, wire, "status={status} with_span={with_span}");
            assert!(
                !encoded.as_object().unwrap().contains_key("identity"),
                "status={status} with_span={with_span}"
            );
        }
    }
}
