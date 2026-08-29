use super::{CollectionWatcherStatus, CollectionWatchersStatusResponse};
use crate::wire_schemas::{
    encode_wire_document as encode_watchers_status_wire_document, wire_content_hash,
    CanonicalIdentity as WatchersStatusCanonicalIdentity,
    WireArtifactKind as WatchersStatusWireArtifactKind,
    WIRE_SCHEMA_VERSION as WATCHERS_STATUS_WIRE_SCHEMA_VERSION,
};
use serde::Serialize as WatchersStatusSerialize;

#[derive(WatchersStatusSerialize)]
struct CollectionWatchersStatusResponseBody<'a> {
    watchers: &'a [CollectionWatcherStatus],
}

fn watcher(collection_name: &str) -> CollectionWatcherStatus {
    CollectionWatcherStatus {
        collection_name: collection_name.into(),
        watch_enabled: true,
        auto_index_enabled: true,
        active: false,
        ignored_by_config: false,
        watched_root_count: 1,
        pending_event_count: 2,
        last_event_at: Some("2025-01-01T00:00:00Z".into()),
        last_sync_at: Some("2025-01-02T00:00:00Z".into()),
        last_error: Some("previous failure".into()),
        last_added: 3,
        last_removed: 4,
        last_unchanged: 5,
        last_task_id: Some("task-1".into()),
    }
}

#[test]
fn collection_watchers_status_result_identity() {
    for watchers in [Vec::new(), vec![watcher("articles"), watcher("books")]] {
        let response = CollectionWatchersStatusResponse::new(watchers.clone()).unwrap();
        let wire = serde_json::to_value(&response).unwrap();
        let expected = WatchersStatusCanonicalIdentity::from_body(
            WatchersStatusWireArtifactKind::CollectionWatchersStatusResult,
            WATCHERS_STATUS_WIRE_SCHEMA_VERSION,
            "collections-watchers-status",
            &encode_watchers_status_wire_document(&CollectionWatchersStatusResponseBody {
                watchers: &watchers,
            })
            .unwrap(),
        )
        .unwrap();

        assert_eq!(
            wire["identity"]["kind"],
            "collection_watchers_status_result"
        );
        assert_eq!(
            wire["identity"]["artifact_id"],
            "collections-watchers-status"
        );
        assert_eq!(
            wire["identity"]["schema_version"],
            serde_json::json!(WATCHERS_STATUS_WIRE_SCHEMA_VERSION)
        );
        assert_eq!(
            wire["identity"]["content_hash"],
            expected.content_hash.as_str()
        );
        assert_eq!(
            serde_json::from_value::<CollectionWatchersStatusResponse>(wire.clone()).unwrap(),
            response
        );

        let mut missing_identity = wire.clone();
        missing_identity.as_object_mut().unwrap().remove("identity");
        assert!(
            serde_json::from_value::<CollectionWatchersStatusResponse>(missing_identity).is_err()
        );

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
                serde_json::from_value::<CollectionWatchersStatusResponse>(mutated).is_err(),
                "identity field {field} mutation must be rejected"
            );
        }

        if !watchers.is_empty() {
            let mut reordered = wire.clone();
            reordered["watchers"].as_array_mut().unwrap().reverse();
            assert!(serde_json::from_value::<CollectionWatchersStatusResponse>(reordered).is_err());

            for (field, value) in [
                ("collection_name", serde_json::json!("other")),
                ("watch_enabled", serde_json::json!(false)),
                ("auto_index_enabled", serde_json::json!(false)),
                ("active", serde_json::json!(true)),
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
                mutated["watchers"][0][field] = value;
                assert!(
                    serde_json::from_value::<CollectionWatchersStatusResponse>(mutated).is_err(),
                    "watcher field {field} mutation must be rejected"
                );
            }
        }
    }
}
