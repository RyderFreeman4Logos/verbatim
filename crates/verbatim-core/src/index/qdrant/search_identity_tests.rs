#[tokio::test]
async fn search_sends_filter_and_keeps_only_complete_matching_identity() {
    let (url, handle) = spawn_server(vec![(
        200,
        r#"{"status":"ok","result":[{"id":"d797400c-780b-51e8-b4fe-956e78c2f01d","score":0.75,"payload":{"chunk_id":"chunk-a","profile_generation":3,"profile_id":"alt","source_id":"src-1"}}]}"#,
    )]);
    let client = QdrantClient::new(qdrant_config(url));
    let alt_profile = EmbeddingProfileId::new("alt").unwrap();
    let source_filter = HashSet::from([SourceId("src-1".into())]);

    let hits = client
        .search(&alt_profile, &[0.3, 0.4], 7, Some(&source_filter))
        .await
        .unwrap();

    assert_eq!(
        hits,
        vec![QdrantHit {
            point_id: "d797400c-780b-51e8-b4fe-956e78c2f01d".into(),
            chunk_id: ChunkId("chunk-a".into()),
            profile_id: alt_profile,
            source_id: SourceId("src-1".into()),
            score: 0.75,
            profile_generation: 3,
        }]
    );
    let requests = handle.join().unwrap();
    assert_eq!(
        requests[0].line,
        "POST /collections/verbatim/points/search HTTP/1.1"
    );
    let body: Value = serde_json::from_str(&requests[0].body).unwrap();
    assert_eq!(body["limit"], 7);
    assert_eq!(body["filter"]["must"][0]["key"], "profile_id");
    assert_eq!(body["filter"]["must"][0]["match"]["value"], "alt");
    assert_eq!(body["filter"]["must"][1]["key"], "source_id");
    assert_eq!(body["filter"]["must"][1]["match"]["value"], "src-1");
    assert_eq!(
        body["with_payload"],
        serde_json::json!(["chunk_id", "profile_generation", "profile_id", "source_id"])
    );
    assert_eq!(body["with_vector"], false);
}

#[tokio::test]
async fn search_preserves_source_filter_shapes_and_sorts_multi_source_any() {
    let (url, handle) = spawn_server(vec![
        (200, r#"{"status":"ok","result":[]}"#),
        (200, r#"{"status":"ok","result":[]}"#),
        (200, r#"{"status":"ok","result":[]}"#),
    ]);
    let client = QdrantClient::new(qdrant_config(url));
    let alt_profile = EmbeddingProfileId::new("alt").unwrap();
    let empty_sources = HashSet::new();
    let single_source = HashSet::from([SourceId("src-1".into())]);
    let mut multiple_sources = HashSet::new();
    multiple_sources.insert(SourceId("src-z".into()));
    multiple_sources.insert(SourceId("src-a".into()));

    assert!(client
        .search(&alt_profile, &[0.3, 0.4], 7, Some(&empty_sources))
        .await
        .unwrap()
        .is_empty());
    client
        .search(&alt_profile, &[0.3, 0.4], 7, None)
        .await
        .unwrap();
    client
        .search(&alt_profile, &[0.3, 0.4], 7, Some(&single_source))
        .await
        .unwrap();
    client
        .search(&alt_profile, &[0.3, 0.4], 7, Some(&multiple_sources))
        .await
        .unwrap();

    let requests = handle.join().unwrap();
    assert_eq!(
        requests[0].body,
        r#"{"vector":[0.3,0.4],"limit":7,"filter":{"must":[{"key":"profile_id","match":{"value":"alt"}}]},"with_payload":["chunk_id","profile_generation","profile_id","source_id"],"with_vector":false}"#
    );
    assert_eq!(
        requests[1].body,
        r#"{"vector":[0.3,0.4],"limit":7,"filter":{"must":[{"key":"profile_id","match":{"value":"alt"}},{"key":"source_id","match":{"value":"src-1"}}]},"with_payload":["chunk_id","profile_generation","profile_id","source_id"],"with_vector":false}"#
    );
    assert_eq!(
        requests[2].body,
        r#"{"vector":[0.3,0.4],"limit":7,"filter":{"must":[{"key":"profile_id","match":{"value":"alt"}},{"key":"source_id","match":{"any":["src-a","src-z"]}}]},"with_payload":["chunk_id","profile_generation","profile_id","source_id"],"with_vector":false}"#
    );
}

#[test]
fn payload_parser_rejects_malformed_or_mismatched_identity() {
    let valid_payload = serde_json::json!({
        "chunk_id": "chunk-a",
        "profile_generation": 3,
        "profile_id": "alt",
        "source_id": "src-1",
    });
    let point_id = "d797400c-780b-51e8-b4fe-956e78c2f01d";
    let cases = [
        (
            Some(serde_json::json!(
                "550e8400-e29b-41d4-a716-446655440000"
            )),
            Some(valid_payload.clone()),
        ),
        (
            Some(serde_json::json!(point_id)),
            Some(serde_json::json!({
                "chunk_id": "chunk-a",
                "profile_generation": 3,
                "profile_id": "alt",
            })),
        ),
        (
            Some(serde_json::json!(point_id)),
            Some(serde_json::json!({
                "chunk_id": "chunk-a",
                "profile_generation": 3,
                "profile_id": "bad profile",
                "source_id": "src-1",
            })),
        ),
        (Some(serde_json::json!(7)), Some(valid_payload)),
    ];

    for (point_id, payload) in cases {
        assert!(hit_from_payload(point_id, payload, 0.9).is_none());
    }
}
