use super::AskErrorEvent;
use crate::wire_schemas::{ContentHash, WireArtifactKind, WireSchemaVersion};

const MODEL_FAILED_HASH: &str = "a23d20252c35ca48f9f740c493f4046698c2026482293ecc93bf8efe13188976";

fn model_failed_wire() -> serde_json::Value {
    serde_json::json!({
        "status": 500,
        "error": "model failed",
        "identity": {
            "kind": "ask_error_event",
            "schema_version": {"major": 1, "minor": 0, "patch": 0},
            "artifact_id": "ask-stream-error",
            "content_hash": MODEL_FAILED_HASH,
        }
    })
}

#[test]
fn ask_error_event_serializes_the_bound_identity() {
    let event = AskErrorEvent::new(Some(500), "model failed").unwrap();

    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        format!(
            "{{\"status\":500,\"error\":\"model failed\",\"identity\":{{\"kind\":\"ask_error_event\",\"schema_version\":{{\"major\":1,\"minor\":0,\"patch\":0}},\"artifact_id\":\"ask-stream-error\",\"content_hash\":\"{MODEL_FAILED_HASH}\"}}}}"
        )
    );
}

#[test]
fn ask_error_event_decodes_a_sibling_wire_fixture() {
    let event: AskErrorEvent = serde_json::from_value(model_failed_wire()).unwrap();

    assert_eq!(event.status, Some(500));
    assert_eq!(event.error, "model failed");
}

fn mutate_status(wire: &mut serde_json::Value) {
    wire["status"] = serde_json::json!(400);
}

fn mutate_error(wire: &mut serde_json::Value) {
    wire["error"] = serde_json::json!("request failed");
}

fn mutate_kind(wire: &mut serde_json::Value) {
    wire["identity"]["kind"] = serde_json::json!("derived_artifact");
}

fn mutate_schema_version(wire: &mut serde_json::Value) {
    wire["identity"]["schema_version"]["major"] = serde_json::json!(2);
}

fn mutate_artifact_id(wire: &mut serde_json::Value) {
    wire["identity"]["artifact_id"] = serde_json::json!("other-error");
}

fn mutate_content_hash(wire: &mut serde_json::Value) {
    wire["identity"]["content_hash"] = serde_json::json!("deadbeef");
}

#[test]
fn ask_error_event_identity_mismatch_is_rejected() {
    for (name, mutation) in [
        ("status", mutate_status as fn(&mut serde_json::Value)),
        ("error", mutate_error),
        ("kind", mutate_kind),
        ("schema_version", mutate_schema_version),
        ("artifact_id", mutate_artifact_id),
        ("content_hash", mutate_content_hash),
    ] {
        let mut wire = model_failed_wire();
        mutation(&mut wire);
        let error = serde_json::from_value::<AskErrorEvent>(wire)
            .expect_err("ask-error-event identity mismatch must fail closed");
        assert!(
            !error.to_string().is_empty(),
            "unexpected ask-error-event identity error for {name}: {error}"
        );
    }
}

#[test]
fn ask_error_event_rejects_mutated_body_and_identity_on_serialize() {
    let mut status = AskErrorEvent::new(Some(500), "model failed").unwrap();
    status.status = Some(400);
    assert!(serde_json::to_value(status).is_err(), "status mutation");

    let mut error = AskErrorEvent::new(Some(500), "model failed").unwrap();
    error.error = "request failed".into();
    assert!(serde_json::to_value(error).is_err(), "error mutation");

    let mut kind = AskErrorEvent::new(Some(500), "model failed").unwrap();
    kind.identity.kind = WireArtifactKind::DerivedArtifact;
    assert!(serde_json::to_value(kind).is_err(), "kind mutation");

    let mut schema_version = AskErrorEvent::new(Some(500), "model failed").unwrap();
    schema_version.identity.schema_version = WireSchemaVersion::new(9, 0, 0);
    assert!(
        serde_json::to_value(schema_version).is_err(),
        "schema version mutation"
    );

    let mut artifact_id = AskErrorEvent::new(Some(500), "model failed").unwrap();
    artifact_id.identity.artifact_id = "other-error".into();
    assert!(
        serde_json::to_value(artifact_id).is_err(),
        "artifact id mutation"
    );

    let mut content_hash = AskErrorEvent::new(Some(500), "model failed").unwrap();
    content_hash.identity.content_hash = ContentHash::new("deadbeef").unwrap();
    assert!(
        serde_json::to_value(content_hash).is_err(),
        "content hash mutation"
    );
}
