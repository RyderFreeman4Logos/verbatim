use super::AskCollectionFilterEvent;
use crate::api::{
    AppliedCollectionFilterResponse, CollectionFilterRequest, CollectionFilterResponse,
};
use crate::wire_schemas::{ContentHash, WireArtifactKind, WireSchemaVersion};

const FILTER_HASH: &str = "7608861ada6af9bc785509c7340a261355b2ba9e014340e6bdcf66ac8f727c21";

fn sample_filter() -> CollectionFilterResponse {
    CollectionFilterResponse {
        requested: CollectionFilterRequest {
            collection_ids: vec!["col-1".into()],
            names: vec!["articles".into()],
            require_fresh: true,
        },
        union_source_count: 2,
        applied: vec![AppliedCollectionFilterResponse {
            collection_id: "col-1".into(),
            name: "articles".into(),
            member_count: 3,
            indexed_member_count: 2,
            stale_member_count: 1,
            last_synced_at: None,
            stale: true,
        }],
        warnings: vec!["collection stale".into()],
        stale: true,
    }
}

fn filter_event() -> AskCollectionFilterEvent {
    AskCollectionFilterEvent::new(sample_filter()).unwrap()
}

fn filter_wire() -> serde_json::Value {
    serde_json::json!({
        "requested": {
            "collection_ids": ["col-1"],
            "names": ["articles"],
            "require_fresh": true,
        },
        "union_source_count": 2,
        "applied": [{
            "collection_id": "col-1",
            "name": "articles",
            "member_count": 3,
            "indexed_member_count": 2,
            "stale_member_count": 1,
            "stale": true,
        }],
        "warnings": ["collection stale"],
        "stale": true,
        "identity": {
            "kind": "ask_collection_filter_event",
            "schema_version": {"major": 1, "minor": 0, "patch": 0},
            "artifact_id": "ask-stream-collection-filter",
            "content_hash": FILTER_HASH,
        }
    })
}

#[test]
fn ask_collection_filter_event_serializes_bound_identity_and_field_order() {
    assert_eq!(
        serde_json::to_string(&filter_event()).unwrap(),
        format!(
            "{{\"requested\":{{\"collection_ids\":[\"col-1\"],\"names\":[\"articles\"],\"require_fresh\":true}},\"union_source_count\":2,\"applied\":[{{\"collection_id\":\"col-1\",\"name\":\"articles\",\"member_count\":3,\"indexed_member_count\":2,\"stale_member_count\":1,\"stale\":true}}],\"warnings\":[\"collection stale\"],\"stale\":true,\"identity\":{{\"kind\":\"ask_collection_filter_event\",\"schema_version\":{{\"major\":1,\"minor\":0,\"patch\":0}},\"artifact_id\":\"ask-stream-collection-filter\",\"content_hash\":\"{FILTER_HASH}\"}}}}"
        )
    );
}

#[test]
fn ask_collection_filter_event_decodes_a_valid_wire_fixture() {
    let event: AskCollectionFilterEvent = serde_json::from_value(filter_wire()).unwrap();

    assert_eq!(event.requested, sample_filter().requested);
    assert_eq!(event.union_source_count, 2);
    assert_eq!(event.applied, sample_filter().applied);
    assert_eq!(event.warnings, vec!["collection stale"]);
    assert!(event.stale);
}

fn mutate_requested(wire: &mut serde_json::Value) {
    wire["requested"]["names"] = serde_json::json!(["other"]);
}

fn mutate_union_source_count(wire: &mut serde_json::Value) {
    wire["union_source_count"] = serde_json::json!(99);
}

fn mutate_applied(wire: &mut serde_json::Value) {
    wire["applied"][0]["member_count"] = serde_json::json!(99);
}

fn mutate_warnings(wire: &mut serde_json::Value) {
    wire["warnings"] = serde_json::json!(["other warning"]);
}

fn mutate_stale(wire: &mut serde_json::Value) {
    wire["stale"] = serde_json::json!(false);
}

fn mutate_kind(wire: &mut serde_json::Value) {
    wire["identity"]["kind"] = serde_json::json!("derived_artifact");
}

fn mutate_schema_version(wire: &mut serde_json::Value) {
    wire["identity"]["schema_version"]["major"] = serde_json::json!(2);
}

fn mutate_artifact_id(wire: &mut serde_json::Value) {
    wire["identity"]["artifact_id"] = serde_json::json!("other-filter");
}

fn mutate_content_hash(wire: &mut serde_json::Value) {
    wire["identity"]["content_hash"] = serde_json::json!("deadbeef");
}

#[test]
fn ask_collection_filter_event_identity_mismatch_is_rejected_on_decode() {
    for (name, mutation) in [
        ("requested", mutate_requested as fn(&mut serde_json::Value)),
        ("union_source_count", mutate_union_source_count),
        ("applied", mutate_applied),
        ("warnings", mutate_warnings),
        ("stale", mutate_stale),
        ("kind", mutate_kind),
        ("schema_version", mutate_schema_version),
        ("artifact_id", mutate_artifact_id),
        ("content_hash", mutate_content_hash),
    ] {
        let mut wire = filter_wire();
        mutation(&mut wire);
        let error = serde_json::from_value::<AskCollectionFilterEvent>(wire)
            .expect_err("ask-collection-filter-event identity mismatch must fail closed");
        assert!(
            !error.to_string().is_empty(),
            "unexpected ask-collection-filter-event identity error for {name}: {error}"
        );
    }
}

#[test]
fn ask_collection_filter_event_rejects_mutated_body_and_identity_on_serialize() {
    let mut requested = filter_event();
    requested.requested.names = vec!["other".into()];
    assert!(
        serde_json::to_value(requested).is_err(),
        "requested mutation"
    );

    let mut union_source_count = filter_event();
    union_source_count.union_source_count = 99;
    assert!(
        serde_json::to_value(union_source_count).is_err(),
        "union source count mutation"
    );

    let mut applied = filter_event();
    applied.applied[0].member_count = 99;
    assert!(serde_json::to_value(applied).is_err(), "applied mutation");

    let mut warnings = filter_event();
    warnings.warnings = vec!["other warning".into()];
    assert!(serde_json::to_value(warnings).is_err(), "warnings mutation");

    let mut stale = filter_event();
    stale.stale = false;
    assert!(serde_json::to_value(stale).is_err(), "stale mutation");

    let mut kind = filter_event();
    kind.identity.kind = WireArtifactKind::DerivedArtifact;
    assert!(serde_json::to_value(kind).is_err(), "kind mutation");

    let mut schema_version = filter_event();
    schema_version.identity.schema_version = WireSchemaVersion::new(9, 0, 0);
    assert!(
        serde_json::to_value(schema_version).is_err(),
        "schema version mutation"
    );

    let mut artifact_id = filter_event();
    artifact_id.identity.artifact_id = "other-filter".into();
    assert!(
        serde_json::to_value(artifact_id).is_err(),
        "artifact id mutation"
    );

    let mut content_hash = filter_event();
    content_hash.identity.content_hash = ContentHash::new("deadbeef").unwrap();
    assert!(
        serde_json::to_value(content_hash).is_err(),
        "content hash mutation"
    );
}
