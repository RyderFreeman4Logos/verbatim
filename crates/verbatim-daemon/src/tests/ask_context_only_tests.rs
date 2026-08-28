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
    let task_id = response.task_id.clone();
    let encoded = serde_json::to_value(&response).unwrap();
    assert_eq!(encoded["task_id"], task_id);
    assert_eq!(encoded["identity"]["kind"], "ask_run");
    assert_eq!(encoded["identity"]["artifact_id"], response.task_id);
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

#[tokio::test]
async fn normal_ask_excludes_ocr_and_generated_candidates_from_prompt_and_output() {
    let model_server = MockModelServer::start_with_chat(3, "Answer from source [E1]").await;
    let test_dir = TestDir::new("ask-excludes-ocr-generated");
    let source_path = test_dir.path().join("source.md");
    fs::write(
        &source_path,
        "Ask source needle is citable evidence for the production ask path.",
    )
    .unwrap();
    let mut config = retrieve_test_config(&model_server.base_url);
    config.embedding.enabled = false;
    config.chat.enabled = true;
    config.chat.base_url.clone_from(&model_server.base_url);
    config.chat.model = "test-chat".into();
    config.retrieval.bm25_top_k = 8;
    config.retrieval.default_limit = 8;

    let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let source_id = pipeline.add_source(&source_path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();

    let source_evidence = pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap()
        .into_iter()
        .find(|unit| unit.kind == EvidenceKind::Text)
        .expect("text evidence");
    let source_chunk = pipeline
        .store()
        .list_chunks_by_source(&source_id)
        .unwrap()
        .into_iter()
        .find(|chunk| chunk.evidence_unit_ids.contains(&source_evidence.id))
        .expect("text chunk");
    let hidden_specs = [
        ("ask-hidden-ocr", EvidenceKind::Ocr, None),
        (
            "ask-hidden-generated",
            EvidenceKind::Generated,
            Some(source_evidence.id.clone()),
        ),
    ];
    let hidden_evidence = hidden_specs
        .iter()
        .map(|(id, kind, derived_from)| {
            let mut evidence = source_evidence.clone();
            evidence.id = EvidenceId((*id).into());
            evidence.kind = *kind;
            evidence.derived_from = derived_from.clone();
            evidence.text = format!("Ask source needle hidden {id} must never reach ask output.");
            evidence.text_hash = verbatim_core::types::hex_sha256(evidence.text.as_bytes());
            evidence
        })
        .collect::<Vec<_>>();
    let hidden_chunks = hidden_evidence
        .iter()
        .map(|evidence| {
            let mut chunk = source_chunk.clone();
            chunk.id = ChunkId(format!("{}-chunk", evidence.id.0));
            chunk.chunk_hash = format!("{}-hash", chunk.id.0);
            chunk.text = evidence.text.clone();
            chunk.evidence_unit_ids = vec![evidence.id.clone()];
            chunk
        })
        .collect::<Vec<_>>();
    pipeline
        .store()
        .bulk_insert_evidence(&hidden_evidence)
        .unwrap();
    pipeline.store().bulk_insert_chunks(&hidden_chunks).unwrap();
    pipeline
        .store()
        .link_chunk_evidence(
            &hidden_chunks
                .iter()
                .zip(&hidden_evidence)
                .map(|(chunk, evidence)| (chunk.id.clone(), evidence.id.clone()))
                .collect::<Vec<_>>(),
        )
        .unwrap();
    pipeline.fts_startup_maintenance();

    let state = test_state(config, test_dir.path(), pipeline);
    let response = ask(
        State(Arc::clone(&state)),
        Json(AskRequest {
            question: "What does the ask source needle establish?".into(),
            source_id: Some(source_id.0.clone()),
            collection_filter: CollectionFilterRequest::default(),
            embedding_profile_id: None,
            show_retrieval: false,
            context_only: false,
            limit: None,
            page_size: None,
            page: None,
        }),
    )
    .await
    .unwrap()
    .0;

    assert!(response.answer.contains("Answer from source"));
    assert!(response
        .citations
        .iter()
        .all(|citation| !citation.evidence_id.starts_with("ask-hidden-")));
    assert!(response.retrieval.is_none());
    let prompt = serde_json::to_string(
        &model_server
            .chat_payloads()
            .into_iter()
            .next()
            .expect("normal ask chat request"),
    )
    .unwrap();
    for marker in [
        "ask-hidden-ocr",
        "ask-hidden-generated",
        "must never reach ask output",
    ] {
        assert!(
            !prompt.contains(marker),
            "hidden candidate leaked into prompt: {prompt}"
        );
    }
    let serialized = serde_json::to_string(&response).unwrap();
    for marker in [
        "ask-hidden-ocr",
        "ask-hidden-generated",
        "must never reach ask output",
    ] {
        assert!(
            !serialized.contains(marker),
            "hidden candidate leaked into output: {serialized}"
        );
    }
}
