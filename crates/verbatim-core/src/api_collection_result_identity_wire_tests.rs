use std::path::PathBuf;

use crate::collection::{CollectionMember, CollectionRecord, CollectionRoot, CollectionRootKind};
use crate::types::SourceId;
use crate::wire_schemas::{encode_wire_document, wire_content_hash, WIRE_SCHEMA_VERSION};
use serde::Serialize;

#[derive(Serialize)]
struct CollectionResponseBody<'a> {
    collection: &'a CollectionRecord,
    roots: &'a [CollectionRoot],
    members: &'a [CollectionMember],
}

fn response(populated: bool) -> super::super::CollectionResponse {
    let collection = CollectionRecord {
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
        last_sync: None,
    };
    let roots = if populated {
        vec![CollectionRoot {
            collection_name: "articles".into(),
            path: PathBuf::from("/tmp/articles"),
            canonical_path: Some(PathBuf::from("/tmp/articles")),
            kind: CollectionRootKind::Directory,
            added_at: "2025-01-01T00:00:00Z".into(),
            updated_at: "2025-01-02T00:00:00Z".into(),
        }]
    } else {
        Vec::new()
    };
    let members = if populated {
        vec![CollectionMember {
            collection_name: "articles".into(),
            source_id: SourceId("source-1".into()),
            logical_path: "guide.md".into(),
            source_path: PathBuf::from("/tmp/articles/guide.md"),
            updated_at: "2025-01-02T00:00:00Z".into(),
        }]
    } else {
        Vec::new()
    };
    super::super::CollectionResponse::new(collection, roots, members).unwrap()
}

fn response_wire(response: &super::super::CollectionResponse) -> serde_json::Value {
    let body = CollectionResponseBody {
        collection: &response.collection,
        roots: &response.roots,
        members: &response.members,
    };
    serde_json::json!({
        "collection": response.collection,
        "roots": response.roots,
        "members": response.members,
        "identity": {
            "kind": "collection_result",
            "schema_version": WIRE_SCHEMA_VERSION,
            "artifact_id": "articles",
            "content_hash": wire_content_hash(&encode_wire_document(&body).unwrap()),
        },
    })
}

#[test]
fn collection_result_identity_is_canonical_and_fail_closed() {
    for populated in [false, true] {
        let response = response(populated);
        let wire = response_wire(&response);
        let decoded =
            serde_json::from_value::<super::super::CollectionResponse>(wire.clone()).unwrap();

        assert_eq!(serde_json::to_value(&decoded).unwrap(), wire);

        let mut missing_identity = wire.clone();
        missing_identity.as_object_mut().unwrap().remove("identity");
        assert!(
            serde_json::from_value::<super::super::CollectionResponse>(missing_identity).is_err()
        );

        let mut collection_mutation = wire.clone();
        collection_mutation["collection"]["name"] = serde_json::json!("other");
        assert!(
            serde_json::from_value::<super::super::CollectionResponse>(collection_mutation)
                .is_err()
        );

        let mut roots_mutation = wire.clone();
        if populated {
            roots_mutation["roots"][0]["path"] = serde_json::json!("/tmp/changed");
        } else {
            roots_mutation["roots"] = serde_json::json!([{
                "collection_name": "articles",
                "path": "/tmp/articles",
                "kind": "directory",
                "added_at": "2025-01-01T00:00:00Z",
                "updated_at": "2025-01-02T00:00:00Z"
            }]);
        }
        assert!(
            serde_json::from_value::<super::super::CollectionResponse>(roots_mutation).is_err()
        );

        let mut members_mutation = wire.clone();
        if populated {
            members_mutation["members"][0]["logical_path"] = serde_json::json!("changed.md");
        } else {
            members_mutation["members"] = serde_json::json!([{
                "collection_name": "articles",
                "source_id": "source-1",
                "logical_path": "guide.md",
                "source_path": "/tmp/articles/guide.md",
                "updated_at": "2025-01-02T00:00:00Z"
            }]);
        }
        assert!(
            serde_json::from_value::<super::super::CollectionResponse>(members_mutation).is_err()
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
            let mut identity_mutation = wire.clone();
            identity_mutation["identity"][field] = value;
            assert!(
                serde_json::from_value::<super::super::CollectionResponse>(identity_mutation)
                    .is_err(),
                "identity field {field} mutation must be rejected"
            );
        }
    }
}
