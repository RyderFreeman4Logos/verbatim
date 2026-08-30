use crate::collection::CollectionRecord;
use crate::wire_schemas::{encode_wire_document, wire_content_hash, WIRE_SCHEMA_VERSION};
use serde::Serialize;

#[derive(Serialize)]
struct CollectionListResponseBody<'a> {
    collections: &'a [CollectionRecord],
}

fn collection(name: &str, watch_enabled: bool) -> CollectionRecord {
    CollectionRecord {
        name: name.into(),
        ignore_patterns: if watch_enabled {
            vec!["drafts/".into()]
        } else {
            Vec::new()
        },
        watch_enabled,
        auto_index_enabled: !watch_enabled,
        created_at: "2025-01-01T00:00:00Z".into(),
        updated_at: "2025-01-02T00:00:00Z".into(),
        last_synced_at: watch_enabled.then(|| "2025-01-03T00:00:00Z".into()),
        last_sync: None,
    }
}

fn response_wire(collections: &[CollectionRecord]) -> serde_json::Value {
    let body = CollectionListResponseBody { collections };
    serde_json::json!({
        "collections": collections,
        "identity": {
            "kind": "collection_list_result",
            "schema_version": WIRE_SCHEMA_VERSION,
            "artifact_id": "collections",
            "content_hash": wire_content_hash(&encode_wire_document(&body).unwrap()),
        },
    })
}

#[test]
fn collection_list_result_identity_is_canonical_and_fail_closed() {
    for collections in [
        Vec::new(),
        vec![collection("articles", true), collection("books", false)],
    ] {
        let wire = response_wire(&collections);
        let decoded = serde_json::from_value::<super::CollectionListResponse>(wire.clone())
            .expect("valid collection-list identity must decode");
        let encoded = serde_json::to_string(&decoded).unwrap();
        assert!(encoded.starts_with("{\"collections\":"));
        assert!(encoded.find("\"identity\"").unwrap() > encoded.find("\"collections\"").unwrap());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&encoded).unwrap(),
            wire
        );

        let mut missing_identity = wire.clone();
        missing_identity.as_object_mut().unwrap().remove("identity");
        assert!(serde_json::from_value::<super::CollectionListResponse>(missing_identity).is_err());

        let mut reordered = wire.clone();
        if collections.len() > 1 {
            reordered["collections"] = serde_json::json!([&collections[1], &collections[0]]);
            assert!(
                serde_json::from_value::<super::CollectionListResponse>(reordered).is_err(),
                "collection order is part of the canonical body"
            );
        }

        let mut collection_mutation = wire.clone();
        if collections.is_empty() {
            collection_mutation["collections"] = serde_json::json!([collection("articles", true)]);
        } else {
            collection_mutation["collections"][0]["name"] = serde_json::json!("changed");
        }
        assert!(
            serde_json::from_value::<super::CollectionListResponse>(collection_mutation).is_err()
        );

        for (field, value) in [
            ("kind", serde_json::json!("collection_result")),
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
                serde_json::from_value::<super::CollectionListResponse>(identity_mutation).is_err(),
                "identity field {field} mutation must be rejected"
            );
        }

        let mut unknown_field = wire;
        unknown_field["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<super::CollectionListResponse>(unknown_field).is_err());
    }
}
