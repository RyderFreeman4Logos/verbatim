use crate::collection::{CollectionRecord, CollectionStatus, CollectionSyncReport};
use crate::wire_schemas::{encode_wire_document, wire_content_hash, WIRE_SCHEMA_VERSION};
use serde::Serialize;

#[derive(Serialize)]
struct CollectionStatusResponseBody<'a> {
    status: &'a CollectionStatus,
}

fn collection_status(populated: bool) -> CollectionStatus {
    CollectionStatus {
        collection: CollectionRecord {
            name: "articles".into(),
            ignore_patterns: if populated {
                vec!["drafts/".into()]
            } else {
                Vec::new()
            },
            watch_enabled: populated,
            auto_index_enabled: !populated,
            created_at: "2025-01-01T00:00:00Z".into(),
            updated_at: "2025-01-02T00:00:00Z".into(),
            last_synced_at: populated.then(|| "2025-01-03T00:00:00Z".into()),
            last_sync: populated.then(|| CollectionSyncReport {
                member_count: 2,
                added: 1,
                removed: 0,
                unchanged: 1,
                scanned_roots: 1,
                max_depth: 32,
                skipped: Vec::new(),
            }),
        },
        root_count: 1,
        member_count: 2,
    }
}

fn response_wire(status: &CollectionStatus) -> serde_json::Value {
    let body = CollectionStatusResponseBody { status };
    serde_json::json!({
        "status": status,
        "identity": {
            "kind": "collection_status_result",
            "schema_version": WIRE_SCHEMA_VERSION,
            "artifact_id": "articles",
            "content_hash": wire_content_hash(&encode_wire_document(&body).unwrap()),
        },
    })
}

#[test]
fn collection_status_result_identity() {
    for populated in [false, true] {
        let status = collection_status(populated);
        let wire = response_wire(&status);
        let response =
            serde_json::from_value::<super::CollectionStatusResponse>(wire.clone()).unwrap();

        assert_eq!(serde_json::to_value(&response).unwrap(), wire);
        let mut missing_identity = wire.clone();
        missing_identity.as_object_mut().unwrap().remove("identity");
        assert!(
            serde_json::from_value::<super::CollectionStatusResponse>(missing_identity).is_err()
        );

        for (field, value) in [
            ("name", serde_json::json!("other")),
            ("ignore_patterns", serde_json::json!(["changed/"])),
            ("watch_enabled", serde_json::json!(!populated)),
            ("auto_index_enabled", serde_json::json!(populated)),
            ("created_at", serde_json::json!("changed")),
            ("updated_at", serde_json::json!("changed")),
            (
                "last_synced_at",
                if populated {
                    serde_json::Value::Null
                } else {
                    serde_json::json!("2025-01-03T00:00:00Z")
                },
            ),
            (
                "last_sync",
                if populated {
                    serde_json::Value::Null
                } else {
                    serde_json::json!({
                        "member_count": 2,
                        "added": 1,
                        "removed": 0,
                        "unchanged": 1,
                        "scanned_roots": 1,
                        "max_depth": 32,
                        "skipped": [],
                    })
                },
            ),
        ] {
            let mut mutated = wire.clone();
            mutated["status"]["collection"][field] = value;
            assert!(
                serde_json::from_value::<super::CollectionStatusResponse>(mutated).is_err(),
                "collection field {field} mutation must be rejected"
            );
        }

        for (field, value) in [
            ("root_count", serde_json::json!(9)),
            ("member_count", serde_json::json!(9)),
        ] {
            let mut mutated = wire.clone();
            mutated["status"][field] = value;
            assert!(
                serde_json::from_value::<super::CollectionStatusResponse>(mutated).is_err(),
                "status field {field} mutation must be rejected"
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
                serde_json::from_value::<super::CollectionStatusResponse>(mutated).is_err(),
                "identity field {field} mutation must be rejected"
            );
        }
    }
}
