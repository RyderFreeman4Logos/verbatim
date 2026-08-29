#[tokio::test]
async fn collection_watchers_status_api_publishes_identity() {
    let test_dir = TestDir::new("collection-watcher-api");
    let config = retrieve_test_config("http://127.0.0.1:9/v1");
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let state = test_state(config, test_dir.path(), pipeline);

    let _ = create_collection(
        State(Arc::clone(&state)),
        Json(CreateCollectionRequest {
            name: "articles".into(),
            ignore_patterns: Vec::new(),
        }),
    )
    .await
    .unwrap();

    let Json(response) = update_collection_watcher(
        State(Arc::clone(&state)),
        Path("articles".into()),
        Json(CollectionWatcherUpdateRequest {
            enabled: true,
            auto_index_enabled: Some(false),
        }),
    )
    .await
    .unwrap();

    assert!(response.collection.watch_enabled);
    assert!(!response.collection.auto_index_enabled);
    assert!(response.watcher.watch_enabled);
    assert!(!response.watcher.auto_index_enabled);

    let Json(single) =
        collection_watcher_status(State(Arc::clone(&state)), Path("articles".into()))
            .await
            .unwrap();
    assert!(single.collection.watch_enabled);

    let Json(all) = list_collection_watcher_statuses(State(Arc::clone(&state)))
        .await
        .unwrap();
    assert_eq!(all.watchers.len(), 1);
    assert_eq!(all.watchers[0].collection_name, "articles");
    assert_eq!(
        all.identity.kind,
        verbatim_core::wire_schemas::WireArtifactKind::CollectionWatchersStatusResult
    );
    assert_eq!(all.identity.artifact_id, "collections-watchers-status");
    assert!(
        serde_json::from_value::<CollectionWatchersStatusResponse>(serde_json::to_value(&all).unwrap())
            .is_ok()
    );
    let mut mismatched_wire = serde_json::to_value(&all).unwrap();
    mismatched_wire["identity"]["artifact_id"] = serde_json::json!("other");
    assert!(serde_json::from_value::<CollectionWatchersStatusResponse>(mismatched_wire).is_err());
}
