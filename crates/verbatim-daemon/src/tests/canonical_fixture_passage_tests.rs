use super::*;

#[tokio::test]
async fn canonical_bible_fixture_ingest_retrieve_passage() {
    let model_server = MockModelServer::start(3).await;
    let test_dir = TestDir::new("canonical-bible-fixture-passage");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../verbatim-core/tests/fixtures/canonical_bible.jsonl");
    assert!(
        fixture.is_file(),
        "required canonical Bible fixture is missing or unreadable: {}",
        fixture.display()
    );

    let config = retrieve_test_config(&model_server.base_url);
    let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let source_id = pipeline.add_source(&fixture).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();
    let state = test_state(config, test_dir.path(), pipeline);

    let response = retrieve(
        State(state),
        Json(RetrieveRequest {
            question: "crown of righteousness".into(),
            source_id: Some(source_id.0.clone()),
            collection_filter: CollectionFilterRequest::default(),
            embedding_profile_id: None,
            limit: Some(3),
            page_size: Some(3),
            page: Some(1),
            fast: true,
            rerank: Some(false),
            dense_top_k: None,
            bm25_top_k: Some(8),
            rerank_top_n: None,
            bypass_cache: false,
            include_debug: false,
            include_debug_packs: false,
            include_locator: true,
            passage: true,
        }),
    )
    .await
    .unwrap()
    .0;

    assert!(response.source_bounded);
    let passage = response
        .results
        .iter()
        .find(|result| result.snippet.contains("crown of righteousness"))
        .expect("canonical fixture passage should be returned");
    assert_eq!(passage.source_id, source_id.0);
    assert_eq!(passage.locator, "2 Timothy 4:7-8");
    assert_eq!(model_server.chat_requests(), 0);
}
