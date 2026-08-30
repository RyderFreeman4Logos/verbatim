use super::AskCitationEvent;
use crate::api::CitationResponse;
use crate::wire_schemas::{ContentHash, WireArtifactKind, WireSchemaVersion};

const CITATION_HASH: &str = "a383f7d7791bbb45d1819b1628a13afb0a5fcc4199d0bf25b595a38dd297a6f0";

fn sample_citation() -> CitationResponse {
    CitationResponse {
        label: "E1".into(),
        evidence_id: "ev-1".into(),
        kind: "original_text".into(),
        role: "original_text".into(),
        derived_from: None,
        collections: Vec::new(),
        locator: "PDF p.1 para.1".into(),
        text_preview: "preview".into(),
    }
}

fn citation_event() -> AskCitationEvent {
    AskCitationEvent::new(vec![sample_citation()], false).unwrap()
}

fn citation_wire() -> serde_json::Value {
    serde_json::json!({
        "citations": [{
            "label": "E1",
            "evidence_id": "ev-1",
            "kind": "original_text",
            "role": "original_text",
            "derived_from": null,
            "locator": "PDF p.1 para.1",
            "text_preview": "preview",
        }],
        "verified": false,
        "identity": {
            "kind": "ask_citation_event",
            "schema_version": {"major": 1, "minor": 0, "patch": 0},
            "artifact_id": "ask-stream-citation",
            "content_hash": CITATION_HASH,
        }
    })
}

#[test]
fn ask_citation_event_serializes_the_bound_identity_and_field_order() {
    let event = citation_event();

    assert_eq!(
        serde_json::to_string(&event).unwrap(),
        format!(
            "{{\"citations\":[{{\"label\":\"E1\",\"evidence_id\":\"ev-1\",\"kind\":\"original_text\",\"role\":\"original_text\",\"derived_from\":null,\"locator\":\"PDF p.1 para.1\",\"text_preview\":\"preview\"}}],\"verified\":false,\"identity\":{{\"kind\":\"ask_citation_event\",\"schema_version\":{{\"major\":1,\"minor\":0,\"patch\":0}},\"artifact_id\":\"ask-stream-citation\",\"content_hash\":\"{CITATION_HASH}\"}}}}"
        )
    );
}

#[test]
fn ask_citation_event_decodes_a_sibling_wire_fixture() {
    let event: AskCitationEvent = serde_json::from_value(citation_wire()).unwrap();

    assert_eq!(event.citations, vec![sample_citation()]);
    assert!(!event.verified);
}

fn mutate_citations(wire: &mut serde_json::Value) {
    wire["citations"][0]["text_preview"] = serde_json::json!("changed");
}

fn mutate_verified(wire: &mut serde_json::Value) {
    wire["verified"] = serde_json::json!(true);
}

fn mutate_kind(wire: &mut serde_json::Value) {
    wire["identity"]["kind"] = serde_json::json!("derived_artifact");
}

fn mutate_schema_version(wire: &mut serde_json::Value) {
    wire["identity"]["schema_version"]["major"] = serde_json::json!(2);
}

fn mutate_artifact_id(wire: &mut serde_json::Value) {
    wire["identity"]["artifact_id"] = serde_json::json!("other-citation");
}

fn mutate_content_hash(wire: &mut serde_json::Value) {
    wire["identity"]["content_hash"] = serde_json::json!("deadbeef");
}

#[test]
fn ask_citation_event_identity_mismatch_is_rejected_on_decode() {
    for (name, mutation) in [
        ("citations", mutate_citations as fn(&mut serde_json::Value)),
        ("verified", mutate_verified),
        ("kind", mutate_kind),
        ("schema_version", mutate_schema_version),
        ("artifact_id", mutate_artifact_id),
        ("content_hash", mutate_content_hash),
    ] {
        let mut wire = citation_wire();
        mutation(&mut wire);
        let error = serde_json::from_value::<AskCitationEvent>(wire)
            .expect_err("ask-citation-event identity mismatch must fail closed");
        assert!(
            !error.to_string().is_empty(),
            "unexpected ask-citation-event identity error for {name}: {error}"
        );
    }
}

#[test]
fn ask_citation_event_rejects_mutated_body_and_identity_on_serialize() {
    let mut citations = citation_event();
    citations.citations[0].text_preview = "changed".into();
    assert!(
        serde_json::to_value(citations).is_err(),
        "citations mutation"
    );

    let mut verified = citation_event();
    verified.verified = true;
    assert!(serde_json::to_value(verified).is_err(), "verified mutation");

    let mut kind = citation_event();
    kind.identity.kind = WireArtifactKind::DerivedArtifact;
    assert!(serde_json::to_value(kind).is_err(), "kind mutation");

    let mut schema_version = citation_event();
    schema_version.identity.schema_version = WireSchemaVersion::new(9, 0, 0);
    assert!(
        serde_json::to_value(schema_version).is_err(),
        "schema version mutation"
    );

    let mut artifact_id = citation_event();
    artifact_id.identity.artifact_id = "other-citation".into();
    assert!(
        serde_json::to_value(artifact_id).is_err(),
        "artifact id mutation"
    );

    let mut content_hash = citation_event();
    content_hash.identity.content_hash = ContentHash::new("deadbeef").unwrap();
    assert!(
        serde_json::to_value(content_hash).is_err(),
        "content hash mutation"
    );
}
