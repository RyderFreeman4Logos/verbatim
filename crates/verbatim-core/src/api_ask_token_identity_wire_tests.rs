use super::AskTokenEvent;
use crate::wire_schemas::{ContentHash, WireArtifactKind, WireSchemaVersion};

const HELLO_HASH: &str = "cbbbdcd27692344de5dbab3abcaba413fb0f45307267de7081401576df1cb176";

fn hello_wire() -> serde_json::Value {
    serde_json::json!({
        "text": "hello",
        "identity": {
            "kind": "ask_token_event",
            "schema_version": {"major": 1, "minor": 0, "patch": 0},
            "artifact_id": "ask-stream-token",
            "content_hash": HELLO_HASH,
        }
    })
}

#[test]
fn ask_token_event_serializes_the_bound_identity() {
    let event = AskTokenEvent::new("hello").unwrap();

    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        format!(
            "{{\"text\":\"hello\",\"identity\":{{\"kind\":\"ask_token_event\",\"schema_version\":{{\"major\":1,\"minor\":0,\"patch\":0}},\"artifact_id\":\"ask-stream-token\",\"content_hash\":\"{HELLO_HASH}\"}}}}"
        )
    );
}

#[test]
fn ask_token_event_decodes_a_sibling_wire_fixture() {
    let event: AskTokenEvent = serde_json::from_value(hello_wire()).unwrap();

    assert_eq!(event.text, "hello");
}

fn mutate_text(wire: &mut serde_json::Value) {
    wire["text"] = serde_json::json!("goodbye");
}

fn mutate_kind(wire: &mut serde_json::Value) {
    wire["identity"]["kind"] = serde_json::json!("derived_artifact");
}

fn mutate_schema_version(wire: &mut serde_json::Value) {
    wire["identity"]["schema_version"]["major"] = serde_json::json!(2);
}

fn mutate_artifact_id(wire: &mut serde_json::Value) {
    wire["identity"]["artifact_id"] = serde_json::json!("other-token");
}

fn mutate_content_hash(wire: &mut serde_json::Value) {
    wire["identity"]["content_hash"] = serde_json::json!("deadbeef");
}

#[test]
fn ask_token_event_identity_mismatch_is_rejected() {
    for (name, mutation) in [
        ("text", mutate_text as fn(&mut serde_json::Value)),
        ("kind", mutate_kind),
        ("schema_version", mutate_schema_version),
        ("artifact_id", mutate_artifact_id),
        ("content_hash", mutate_content_hash),
    ] {
        let mut wire = hello_wire();
        mutation(&mut wire);
        let error = serde_json::from_value::<AskTokenEvent>(wire)
            .expect_err("ask-token-event identity mismatch must fail closed");
        assert!(
            !error.to_string().is_empty(),
            "unexpected ask-token-event identity error for {name}: {error}"
        );
    }
}

#[test]
fn ask_token_event_rejects_mutated_body_and_identity_on_serialize() {
    let mut text = AskTokenEvent::new("hello").unwrap();
    text.text = "goodbye".into();
    assert!(serde_json::to_value(text).is_err(), "text mutation");

    let mut kind = AskTokenEvent::new("hello").unwrap();
    kind.identity.kind = WireArtifactKind::DerivedArtifact;
    assert!(serde_json::to_value(kind).is_err(), "kind mutation");

    let mut schema_version = AskTokenEvent::new("hello").unwrap();
    schema_version.identity.schema_version = WireSchemaVersion::new(9, 0, 0);
    assert!(
        serde_json::to_value(schema_version).is_err(),
        "schema version mutation"
    );

    let mut artifact_id = AskTokenEvent::new("hello").unwrap();
    artifact_id.identity.artifact_id = "other-token".into();
    assert!(
        serde_json::to_value(artifact_id).is_err(),
        "artifact id mutation"
    );

    let mut content_hash = AskTokenEvent::new("hello").unwrap();
    content_hash.identity.content_hash = ContentHash::new("deadbeef").unwrap();
    assert!(
        serde_json::to_value(content_hash).is_err(),
        "content hash mutation"
    );
}
