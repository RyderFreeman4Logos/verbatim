use crate::collection::{CollectionRecord, CollectionSyncReport};
use crate::wire_schemas::{encode_wire_document, wire_content_hash, WIRE_SCHEMA_VERSION};
use serde::Serialize;

use crate::api::{CollectionWatcherResponse, CollectionWatcherStatus};

#[derive(Serialize)]
struct CollectionWatcherResponseBody<'a> {
    collection: &'a CollectionRecord,
    watcher: &'a CollectionWatcherStatus,
}

fn collection() -> CollectionRecord {
    CollectionRecord {
        name: "articles".into(),
        ignore_patterns: vec!["drafts/".into()],
        watch_enabled: true,
        auto_index_enabled: false,
        created_at: "2025-01-01T00:00:00Z".into(),
        updated_at: "2025-01-02T00:00:00Z".into(),
        last_synced_at: Some("2025-01-03T00:00:00Z".into()),
        last_sync: Some(CollectionSyncReport {
            member_count: 2,
            added: 1,
            removed: 0,
            unchanged: 1,
            scanned_roots: 1,
            max_depth: 32,
            skipped: Vec::new(),
        }),
    }
}

fn watcher() -> CollectionWatcherStatus {
    CollectionWatcherStatus {
        collection_name: "articles".into(),
        watch_enabled: true,
        auto_index_enabled: false,
        active: true,
        ignored_by_config: false,
        watched_root_count: 1,
        pending_event_count: 2,
        last_event_at: Some("2025-01-04T00:00:00Z".into()),
        last_sync_at: Some("2025-01-05T00:00:00Z".into()),
        last_error: Some("previous failure".into()),
        last_added: 3,
        last_removed: 4,
        last_unchanged: 5,
        last_task_id: Some("task-1".into()),
    }
}

fn response_wire(response: &CollectionWatcherResponse) -> serde_json::Value {
    let body = CollectionWatcherResponseBody {
        collection: &response.collection,
        watcher: &response.watcher,
    };
    serde_json::json!({
        "collection": response.collection,
        "watcher": response.watcher,
        "identity": {
            "kind": "collection_watcher_result",
            "schema_version": WIRE_SCHEMA_VERSION,
            "artifact_id": "articles",
            "content_hash": wire_content_hash(&encode_wire_document(&body).unwrap()),
        },
    })
}

#[test]
fn collection_watcher_result_identity() {
    let response = CollectionWatcherResponse::new("articles", collection(), watcher()).unwrap();
    assert!(response.validate_for_collection("articles").is_ok());
    assert!(response.validate_for_collection("other").is_err());

    let wire = response_wire(&response);
    assert_eq!(wire["identity"]["kind"], "collection_watcher_result");
    assert_eq!(
        wire["identity"]["schema_version"],
        serde_json::json!(WIRE_SCHEMA_VERSION)
    );
    assert_eq!(wire["identity"]["artifact_id"], "articles");
    assert_eq!(
        wire["identity"]["content_hash"],
        response.identity.content_hash.as_str()
    );
    assert_eq!(
        serde_json::from_value::<CollectionWatcherResponse>(wire.clone()).unwrap(),
        response
    );

    let mut missing_identity = wire.clone();
    missing_identity.as_object_mut().unwrap().remove("identity");
    assert!(serde_json::from_value::<CollectionWatcherResponse>(missing_identity).is_err());

    for (field, value) in [
        ("name", serde_json::json!("other")),
        ("ignore_patterns", serde_json::json!(["changed/"])),
        ("watch_enabled", serde_json::json!(false)),
        ("auto_index_enabled", serde_json::json!(true)),
        ("created_at", serde_json::json!("changed")),
        ("updated_at", serde_json::json!("changed")),
        ("last_synced_at", serde_json::json!("changed")),
        (
            "last_sync",
            serde_json::json!({
                "member_count": 9,
                "added": 9,
                "removed": 9,
                "unchanged": 9,
                "scanned_roots": 9,
                "max_depth": 9,
                "skipped": [],
            }),
        ),
    ] {
        let mut mutated = wire.clone();
        mutated["collection"][field] = value;
        assert!(
            serde_json::from_value::<CollectionWatcherResponse>(mutated).is_err(),
            "collection field {field} mutation must be rejected"
        );
    }

    for (field, value) in [
        ("collection_name", serde_json::json!("other")),
        ("watch_enabled", serde_json::json!(false)),
        ("auto_index_enabled", serde_json::json!(true)),
        ("active", serde_json::json!(false)),
        ("ignored_by_config", serde_json::json!(true)),
        ("watched_root_count", serde_json::json!(9)),
        ("pending_event_count", serde_json::json!(9)),
        ("last_event_at", serde_json::json!("changed")),
        ("last_sync_at", serde_json::json!("changed")),
        ("last_error", serde_json::json!("changed")),
        ("last_added", serde_json::json!(9)),
        ("last_removed", serde_json::json!(9)),
        ("last_unchanged", serde_json::json!(9)),
        ("last_task_id", serde_json::json!("other")),
    ] {
        let mut mutated = wire.clone();
        mutated["watcher"][field] = value;
        assert!(
            serde_json::from_value::<CollectionWatcherResponse>(mutated).is_err(),
            "watcher field {field} mutation must be rejected"
        );
    }

    for (field, value) in [
        ("kind", serde_json::json!("source_record")),
        (
            "schema_version",
            serde_json::json!({"major": 2, "minor": 0, "patch": 0}),
        ),
        ("artifact_id", serde_json::json!("other")),
        (
            "content_hash",
            serde_json::json!(wire_content_hash(&[0; 1])),
        ),
    ] {
        let mut mutated = wire.clone();
        mutated["identity"][field] = value;
        assert!(
            serde_json::from_value::<CollectionWatcherResponse>(mutated).is_err(),
            "identity field {field} mutation must be rejected"
        );
    }

    let mut invalid = response.clone();
    invalid.identity.artifact_id = "other".into();
    assert!(serde_json::to_value(invalid).is_err());
}
