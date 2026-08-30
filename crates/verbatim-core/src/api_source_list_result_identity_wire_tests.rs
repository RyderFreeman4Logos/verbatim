use crate::api::SourceResponse;
use crate::wire_schemas::{encode_wire_document, wire_content_hash, WIRE_SCHEMA_VERSION};
use serde::Serialize;

#[derive(Serialize)]
struct SourceListResponseBody<'a> {
    sources: &'a [SourceResponse],
}

fn source(id: &str) -> SourceResponse {
    SourceResponse::new(
        id,
        format!("/tmp/{id}.md"),
        "Ready",
        format!("hash-{id}"),
        Some("markdown".into()),
        None,
        None,
    )
    .unwrap()
}

fn response_wire(sources: &[SourceResponse]) -> serde_json::Value {
    let body = SourceListResponseBody { sources };
    serde_json::json!({
        "sources": sources,
        "identity": {
            "kind": "source_list_result",
            "schema_version": WIRE_SCHEMA_VERSION,
            "artifact_id": "sources",
            "content_hash": wire_content_hash(&encode_wire_document(&body).unwrap()),
        },
    })
}

#[test]
fn source_list_result_identity_is_canonical_and_fail_closed() {
    assert_eq!(
        crate::wire_schemas::WireArtifactKind::SourceListResult.as_str(),
        "source_list_result"
    );

    for sources in [Vec::new(), vec![source("first"), source("second")]] {
        let wire = response_wire(&sources);
        let decoded = serde_json::from_value::<super::SourceListResponse>(wire.clone())
            .expect("valid source-list identity must decode");
        let encoded = serde_json::to_string(&decoded).unwrap();
        assert!(encoded.starts_with("{\"sources\":"));
        assert!(encoded.find("\"identity\"").unwrap() > encoded.find("\"sources\"").unwrap());
        assert_eq!(
            serde_json::from_str::<serde_json::Value>(&encoded).unwrap(),
            wire
        );
        for nested in &decoded.sources {
            nested.identity.validate().unwrap();
        }

        let mut missing_identity = wire.clone();
        missing_identity.as_object_mut().unwrap().remove("identity");
        assert!(serde_json::from_value::<super::SourceListResponse>(missing_identity).is_err());

        let mut unknown_field = wire.clone();
        unknown_field["unexpected"] = serde_json::json!(true);
        assert!(serde_json::from_value::<super::SourceListResponse>(unknown_field).is_err());

        let mut stale_serialization = decoded.clone();
        stale_serialization.identity = serde_json::from_value(serde_json::json!({
            "kind": "source_list_result",
            "schema_version": WIRE_SCHEMA_VERSION,
            "artifact_id": "sources",
            "content_hash": wire_content_hash(&[0; 1]),
        }))
        .unwrap();
        assert!(serde_json::to_value(stale_serialization).is_err());

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
                serde_json::from_value::<super::SourceListResponse>(identity_mutation).is_err(),
                "identity field {field} mutation must be rejected"
            );
        }

        let mut nested_replacement = wire.clone();
        if sources.is_empty() {
            nested_replacement["sources"] = serde_json::json!([source("replacement")]);
        } else {
            nested_replacement["sources"][0] = serde_json::to_value(source("replacement")).unwrap();
        }
        assert!(
            serde_json::from_value::<super::SourceListResponse>(nested_replacement).is_err(),
            "an independently valid nested source with a stale outer identity must be rejected"
        );

        if sources.len() > 1 {
            let mut reordered = wire.clone();
            reordered["sources"] = serde_json::json!([&sources[1], &sources[0]]);
            assert!(
                serde_json::from_value::<super::SourceListResponse>(reordered).is_err(),
                "source order is part of the canonical body"
            );
        }
    }

    assert!(serde_json::from_value::<super::SourceListResponse>(serde_json::json!([])).is_err());
}
