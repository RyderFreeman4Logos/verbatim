use super::*;

use verbatim_core::traits::Parser;

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

#[tokio::test]
async fn canonical_bible_fixture_language_survives_persist_and_inspect() {
    let test_dir = TestDir::new("canonical-bible-fixture-language");
    let fixture = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../verbatim-core/tests/fixtures/canonical_bible.jsonl");

    // Persist the parsed fixture straight to the store: no live embedding provider needed.
    let config = retrieve_test_config("http://127.0.0.1:1");
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let source_id = pipeline.add_source(&fixture).unwrap();
    let units = verbatim_core::parser::canonical_jsonl::CanonicalJsonlParser
        .parse(&fixture)
        .unwrap();
    pipeline.store().bulk_insert_evidence(&units).unwrap();
    let state = test_state(config, test_dir.path(), pipeline);

    let store = state.task_store.lock().unwrap();
    let evidence = store.list_evidence_by_source(&source_id).unwrap();
    let find = |display: &str| {
        evidence
            .iter()
            .find(|unit| {
                matches!(
                    &unit.locator,
                    SourceLocator::Canonical { locator } if locator.display == display
                )
            })
            .unwrap_or_else(|| panic!("fixture must include {display}"))
    };
    assert_eq!(find("Revelation 7:9").language.as_deref(), Some("el"));
    assert_eq!(
        find("Genesis 1:1").language,
        None,
        "absent language stays absent"
    );
    let revelation_id = find("Revelation 7:9").id.0.clone();
    drop(store);

    // The public evidence inspection route re-exposes the persisted language.
    let response = get_evidence(State(state), Path(revelation_id))
        .await
        .unwrap()
        .0;
    assert_eq!(response.language.as_deref(), Some("el"));
}
