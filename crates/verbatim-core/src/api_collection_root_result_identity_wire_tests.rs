use super::AddCollectionRootResponse;
use crate::collection::{CollectionRoot, CollectionRootKind};
use crate::wire_schemas::{encode_wire_document, wire_content_hash, WIRE_SCHEMA_VERSION};
use serde::Serialize;
use serde_json::Value;

#[derive(Serialize)]
struct CollectionRootResultResponseBody<'a> {
    collection_name: &'a str,
    root: &'a CollectionRoot,
    root_count: usize,
    member_count: usize,
    added: bool,
}

fn root(canonical_path: Option<&str>) -> CollectionRoot {
    CollectionRoot {
        collection_name: "articles".into(),
        path: "/tmp/articles".into(),
        canonical_path: canonical_path.map(Into::into),
        kind: CollectionRootKind::Directory,
        added_at: "2025-01-01T00:00:00Z".into(),
        updated_at: "2025-01-02T00:00:00Z".into(),
    }
}

fn response_wire(added: bool, root: &CollectionRoot) -> Value {
    let body = CollectionRootResultResponseBody {
        collection_name: "articles",
        root,
        root_count: 1,
        member_count: 2,
        added,
    };
    serde_json::json!({
        "collection_name": "articles",
        "root": root,
        "root_count": 1,
        "member_count": 2,
        "added": added,
        "identity": {
            "kind": "collection_root_result",
            "schema_version": WIRE_SCHEMA_VERSION,
            "artifact_id": "articles",
            "content_hash": wire_content_hash(&encode_wire_document(&body).unwrap()),
        },
    })
}

#[test]
fn collection_root_result_identity() {
    for (added, canonical_path) in [(false, None), (true, Some("/srv/articles"))] {
        let root = root(canonical_path);
        let wire = response_wire(added, &root);
        let response = serde_json::from_value::<AddCollectionRootResponse>(wire.clone()).unwrap();
        let produced =
            AddCollectionRootResponse::new("articles", root.clone(), 1, 2, added).unwrap();

        assert_eq!(serde_json::to_value(produced).unwrap(), wire);
        assert_eq!(serde_json::to_value(&response).unwrap(), wire);
        let mut missing_identity = wire.clone();
        missing_identity.as_object_mut().unwrap().remove("identity");
        assert!(serde_json::from_value::<AddCollectionRootResponse>(missing_identity).is_err());

        for (field, value) in [
            ("collection_name", serde_json::json!("other")),
            ("root_count", serde_json::json!(9)),
            ("member_count", serde_json::json!(9)),
            ("added", serde_json::json!(!added)),
        ] {
            let mut mutated = wire.clone();
            mutated[field] = value;
            assert!(
                serde_json::from_value::<AddCollectionRootResponse>(mutated).is_err(),
                "response field {field} mutation must be rejected"
            );
        }

        for (field, value) in [
            ("collection_name", serde_json::json!("other")),
            ("path", serde_json::json!("/tmp/changed")),
            (
                "canonical_path",
                canonical_path.map_or_else(|| serde_json::json!("/srv/articles"), |_| Value::Null),
            ),
            ("kind", serde_json::json!("file")),
            ("added_at", serde_json::json!("changed")),
            ("updated_at", serde_json::json!("changed")),
        ] {
            let mut mutated = wire.clone();
            mutated["root"][field] = value;
            assert!(
                serde_json::from_value::<AddCollectionRootResponse>(mutated).is_err(),
                "root field {field} mutation must be rejected"
            );
        }

        for (field, value) in [
            ("kind", serde_json::json!("collection_sync_result")),
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
                serde_json::from_value::<AddCollectionRootResponse>(mutated).is_err(),
                "identity field {field} mutation must be rejected"
            );
        }
    }
}
