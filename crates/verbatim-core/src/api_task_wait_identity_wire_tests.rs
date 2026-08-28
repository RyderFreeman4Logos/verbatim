use super::TaskWaitEvent;
use crate::task::{TaskId, TaskKind, TaskSpan, TaskSummary};
use crate::wire_schemas::{encode_wire_document, wire_content_hash};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Serialize)]
struct TaskRunIdentityBody<'a> {
    task: &'a TaskSummary,
    spans: &'a [TaskSpan],
}

fn valid_task_wait_event(status: &str, with_span: bool) -> Value {
    let task = TaskSummary {
        id: TaskId("task-fixture".into()),
        kind: TaskKind::Ask,
        status: serde_json::from_value(json!(status)).expect("task status fixture"),
        created_at: "1".into(),
        updated_at: "2".into(),
        started_at: Some("1".into()),
        finished_at: (status == "succeeded").then(|| "2".into()),
        request: json!({"question_chars": 14}),
        result: (status == "succeeded").then(|| json!({"citation_count": 1})),
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
    let body = TaskRunIdentityBody {
        task: &task,
        spans: &spans,
    };
    let body_bytes = encode_wire_document(&body).expect("body fixture encodes");
    json!({
        "task": serde_json::to_value(&task).expect("task fixture encodes"),
        "events": [],
        "spans": serde_json::to_value(&spans).expect("span fixture encodes"),
        "terminal": status == "succeeded",
        "identity": {
            "kind": "task_run",
            "schema_version": {"major": 1, "minor": 0, "patch": 0},
            "artifact_id": "task-fixture",
            "content_hash": wire_content_hash(&body_bytes)
        }
    })
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

fn mutate_span_phase(wire: &mut Value) {
    wire["spans"][0]["phase"] = json!("chat");
}

fn mutate_identity_kind(wire: &mut Value) {
    wire["identity"]["kind"] = json!("task_profile");
}

fn mutate_identity_schema_version(wire: &mut Value) {
    wire["identity"]["schema_version"]["patch"] = json!(1);
}

fn mutate_identity_artifact_id(wire: &mut Value) {
    wire["identity"]["artifact_id"] = json!("task-other");
}

fn mutate_identity_content_hash(wire: &mut Value) {
    wire["identity"]["content_hash"] = json!("not-the-body-hash");
}

#[test]
fn task_wait_event_stamps_task_run_identity_for_running_and_terminal_frames() {
    for (status, with_span) in [("running", false), ("succeeded", true)] {
        let wire = valid_task_wait_event(status, with_span);
        let event: TaskWaitEvent =
            serde_json::from_value(wire.clone()).expect("valid task wait fixture decodes");
        let encoded = serde_json::to_value(event).expect("task wait fixture encodes");

        assert_eq!(encoded["identity"]["kind"], "task_run");
        assert_eq!(encoded["identity"]["artifact_id"], "task-fixture");
        assert_eq!(
            encoded["identity"]["schema_version"],
            json!({"major": 1, "minor": 0, "patch": 0})
        );
        assert_eq!(
            encoded["identity"]["content_hash"],
            wire["identity"]["content_hash"]
        );
        assert_eq!(encoded["terminal"], json!(status == "succeeded"));
    }
}

#[test]
fn task_wait_event_rejects_missing_and_mismatched_identity_components() {
    let mut missing = valid_task_wait_event("succeeded", true);
    missing
        .as_object_mut()
        .expect("task wait fixture is an object")
        .remove("identity");
    assert!(serde_json::from_value::<TaskWaitEvent>(missing).is_err());

    for (name, mutate) in [
        ("task_status", mutate_task_status as fn(&mut Value)),
        ("task_id", mutate_task_id),
        ("span_metadata", mutate_span_metadata),
        ("span_phase", mutate_span_phase),
        ("identity_kind", mutate_identity_kind),
        ("identity_schema_version", mutate_identity_schema_version),
        ("identity_artifact_id", mutate_identity_artifact_id),
        ("identity_content_hash", mutate_identity_content_hash),
    ] {
        let mut wire = valid_task_wait_event("succeeded", true);
        mutate(&mut wire);
        assert!(
            serde_json::from_value::<TaskWaitEvent>(wire).is_err(),
            "task wait identity mismatch must reject: {name}"
        );
    }
}

#[test]
fn task_wait_identity_excludes_event_delta_and_terminal_marker() {
    let mut wire = valid_task_wait_event("succeeded", true);
    wire["events"] = json!([{
        "sequence": 2,
        "task_id": "task-fixture",
        "event_type": "phase",
        "message": "done",
        "payload": {"result_count": 1},
        "created_at": "2"
    }]);
    wire["terminal"] = json!(false);

    let event: TaskWaitEvent = serde_json::from_value(wire.clone()).expect("fixture decodes");
    let encoded = serde_json::to_value(event).expect("fixture encodes");
    assert_eq!(encoded["identity"], wire["identity"]);
    assert_eq!(encoded["events"], wire["events"]);
    assert!(!encoded["terminal"].as_bool().expect("terminal bool"));
}
