use super::*;

#[tokio::test]
async fn ask_context_only_handler_uses_collection_filter() {
    use verbatim_core::collection::{CollectionMemberCandidate, CollectionSyncReport};

    let model_server = MockModelServer::start(3).await;
    let test_dir = TestDir::new("ask-context-only-collection-filter");
    let inside_path = test_dir.path().join("inside.md");
    let outside_path = test_dir.path().join("outside.md");
    fs::write(&inside_path, "Alpha collection-scoped ask evidence.").unwrap();
    fs::write(
        &outside_path,
        "Alpha outside evidence must not appear in collection ask.",
    )
    .unwrap();
    let config = retrieve_test_config(&model_server.base_url);
    let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let inside_id = pipeline.add_source(&inside_path).unwrap();
    let outside_id = pipeline.add_source(&outside_path).unwrap();
    pipeline.ingest_source(&inside_id).await.unwrap();
    pipeline.ingest_source(&outside_id).await.unwrap();
    pipeline.store().create_collection("articles", &[]).unwrap();
    pipeline
        .store()
        .replace_collection_members(
            "articles",
            &[CollectionMemberCandidate {
                source_id: inside_id.clone(),
                logical_path: "inside.md".into(),
                source_path: fs::canonicalize(&inside_path).unwrap(),
            }],
            CollectionSyncReport {
                member_count: 0,
                added: 0,
                removed: 0,
                unchanged: 0,
                scanned_roots: 1,
                max_depth: 32,
                skipped: Vec::new(),
            },
        )
        .unwrap();
    let inside_id_text = inside_id.0.clone();
    let outside_id_text = outside_id.0.clone();
    let state = test_state(config, test_dir.path(), pipeline);

    let response = ask(
        State(state),
        Json(AskRequest {
            question: "Alpha ask evidence?".into(),
            source_id: None,
            collection_filter: CollectionFilterRequest {
                collection_ids: Vec::new(),
                names: vec!["articles".into()],
                require_fresh: false,
            },
            embedding_profile_id: None,
            show_retrieval: false,
            context_only: true,
            limit: None,
            page_size: None,
            page: None,
        }),
    )
    .await
    .unwrap()
    .0;

    assert!(response.collection_filter.is_some());
    let context = response.context.expect("context pack");
    assert!(context
        .results
        .iter()
        .all(|result| result.source_id != outside_id_text));
    let result = context
        .results
        .iter()
        .find(|result| result.source_id == inside_id_text)
        .expect("inside collection result");
    assert_eq!(result.collections[0].name, "articles");
}

#[tokio::test]
async fn ask_context_only_returns_context_pack_when_chat_is_disabled_and_unavailable() {
    let model_server = MockModelServer::start(3).await;
    let test_dir = TestDir::new("ask-context-only-chat-disabled");
    let source_path = test_dir.path().join("doc.md");
    fs::write(
        &source_path,
        "Beta retrieval evidence answers the context-only ask question.",
    )
    .unwrap();
    let config = retrieve_test_config(&model_server.base_url);
    assert!(!config.chat.enabled);

    let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let source_id = pipeline.add_source(&source_path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();

    let state = test_state(config, test_dir.path(), pipeline);
    let response = ask(
        State(state),
        Json(AskRequest {
            question: "Beta context-only question?".into(),
            source_id: Some(source_id.0.clone()),
            collection_filter: CollectionFilterRequest::default(),
            embedding_profile_id: None,
            show_retrieval: false,
            context_only: true,
            limit: None,
            page_size: None,
            page: None,
        }),
    )
    .await
    .unwrap()
    .0;

    assert!(response.answer.is_empty());
    assert!(response.generated_interpretation.is_none());
    assert!(response.citations.is_empty());
    assert!(!response.verified);
    assert!(response.retrieval.is_none());
    let context = response.context.expect("context pack");
    assert_eq!(context.source_id.as_deref(), Some(source_id.0.as_str()));
    assert_eq!(context.returned_results, 1);
    assert!(context.source_bounded);
    assert_eq!(context.results[0].label, "E1");
    assert!(!context.results[0].text_hash.is_empty());
    assert!(context.results[0]
        .snippet
        .contains("Beta retrieval evidence"));
    assert!(context.results[0].structured_locator.is_some());
    assert!(model_server.embedding_requests() >= 2);
    assert_eq!(model_server.chat_requests(), 0);
}
