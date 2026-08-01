use super::*;

#[tokio::test]
async fn populated_db_retrieve_uses_bm25_after_startup() {
    let test_dir = TestDir::new("populated-db-retrieve-bm25-startup");
    let store = Store::new(&test_dir.path().join("verbatim.db")).unwrap();
    let chunk_ids = insert_populated_bm25_startup_fixture(&store, test_dir.path());
    let expected_child_rows = u64::try_from(chunk_ids.len()).unwrap();
    assert_eq!(store.list_sources().unwrap().len(), 2);
    assert_eq!(store.list_child_chunks().unwrap().len(), chunk_ids.len());
    drop(store);

    let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
    config.embedding.enabled = false;
    config.rerank.enabled = false;

    {
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let outcome = pipeline.fts_startup_maintenance();
        assert_eq!(outcome.status, FtsMaintenanceStatus::Rebuilt);
        assert_eq!(
            outcome.reason,
            FtsMaintenanceReason::MissingProjectionVersion
        );
        assert_eq!(outcome.counts.child_rows, expected_child_rows);
        assert_eq!(outcome.counts.fts_rows, expected_child_rows);

        let (response, state) =
            retrieve_populated_bm25_startup_fixture(&config, test_dir.path(), pipeline).await;
        let summary = task_summary_response(&state, TaskId(response.task_id.clone()))
            .await
            .unwrap();
        assert_populated_bm25_startup_response(&response, &summary, chunk_ids.len());
    }

    {
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let outcome = pipeline.fts_startup_maintenance();
        assert_eq!(outcome.status, FtsMaintenanceStatus::Skipped);
        assert_eq!(outcome.reason, FtsMaintenanceReason::Current);
        assert_eq!(outcome.counts.child_rows, expected_child_rows);
        assert_eq!(outcome.counts.fts_rows, expected_child_rows);

        let (response, state) =
            retrieve_populated_bm25_startup_fixture(&config, test_dir.path(), pipeline).await;
        let summary = task_summary_response(&state, TaskId(response.task_id.clone()))
            .await
            .unwrap();
        assert_populated_bm25_startup_response(&response, &summary, chunk_ids.len());
    }
}

fn insert_populated_bm25_startup_fixture(store: &Store, root: &FsPath) -> Vec<ChunkId> {
    let first_path = root.join("startup-alpha.md");
    let second_path = root.join("startup-beta.md");
    fs::write(
        &first_path,
        "startupneedle alpha first chunk\nstartupneedle alpha second chunk\n",
    )
    .unwrap();
    fs::write(&second_path, "startupneedle beta third chunk\n").unwrap();

    let first_source = populated_bm25_source("startup-src-alpha", &first_path);
    let second_source = populated_bm25_source("startup-src-beta", &second_path);
    store.add_source(&first_source).unwrap();
    store.add_source(&second_source).unwrap();

    let evidence = vec![
        populated_bm25_evidence(
            &first_source.id,
            "startup-ev-alpha-1",
            &first_path,
            "startupneedle alpha first chunk",
            0,
        ),
        populated_bm25_evidence(
            &first_source.id,
            "startup-ev-alpha-2",
            &first_path,
            "startupneedle alpha second chunk",
            1,
        ),
        populated_bm25_evidence(
            &second_source.id,
            "startup-ev-beta-1",
            &second_path,
            "startupneedle beta third chunk",
            0,
        ),
    ];
    store.bulk_insert_evidence(&evidence).unwrap();

    let chunks = vec![
        populated_bm25_child(
            &first_source.id,
            "startup-chunk-alpha-1",
            &evidence[0].id,
            "startupneedle alpha first chunk",
        ),
        populated_bm25_child(
            &first_source.id,
            "startup-chunk-alpha-2",
            &evidence[1].id,
            "startupneedle alpha second chunk",
        ),
        populated_bm25_child(
            &second_source.id,
            "startup-chunk-beta-1",
            &evidence[2].id,
            "startupneedle beta third chunk",
        ),
    ];
    let chunk_ids = chunks
        .iter()
        .map(|chunk| chunk.id.clone())
        .collect::<Vec<_>>();
    let links = chunks
        .iter()
        .zip(evidence.iter())
        .map(|(chunk, evidence)| (chunk.id.clone(), evidence.id.clone()))
        .collect::<Vec<_>>();
    store.bulk_insert_chunks(&chunks).unwrap();
    store.link_chunk_evidence(&links).unwrap();

    chunk_ids
}

fn populated_bm25_source(id: &str, path: &FsPath) -> Source {
    Source {
        id: SourceId(id.into()),
        path: path.to_path_buf(),
        hash: format!("hash-{id}"),
        status: SourceStatus::Indexed,
        parser_used: Some("plaintext".into()),
        last_ingested_at: None,
    }
}

fn populated_bm25_evidence(
    source_id: &SourceId,
    id: &str,
    path: &FsPath,
    text: &str,
    position: u32,
) -> EvidenceUnit {
    EvidenceUnit {
        id: EvidenceId(id.into()),
        source_id: source_id.clone(),
        kind: EvidenceKind::Text,
        derived_from: None,
        locator: SourceLocator::Document {
            path_or_url: path.display().to_string(),
            line_start: position.saturating_add(1),
            line_end: None,
        },
        text: text.into(),
        text_hash: format!("hash-{id}"),
        heading_path: vec!["Startup".into()],
        position,
    }
}

fn populated_bm25_child(
    source_id: &SourceId,
    id: &str,
    evidence_id: &EvidenceId,
    text: &str,
) -> Chunk {
    Chunk {
        id: ChunkId(id.into()),
        source_id: source_id.clone(),
        chunk_hash: format!("hash-{id}"),
        embedding_input_hash: None,
        text: text.into(),
        context_text: None,
        token_count: 4,
        chunk_type: ChunkType::Child,
        parent_chunk_id: None,
        heading_path: vec!["Startup".into()],
        evidence_unit_ids: vec![evidence_id.clone()],
    }
}

async fn retrieve_populated_bm25_startup_fixture(
    config: &Config,
    data_dir: &FsPath,
    pipeline: IngestPipeline,
) -> (RetrieveResponse, SharedState) {
    let state = test_state(config.clone(), data_dir, pipeline);
    let response = retrieve(
        State(Arc::clone(&state)),
        Json(RetrieveRequest {
            question: "startupneedle".into(),
            source_id: None,
            collection_filter: CollectionFilterRequest::default(),
            embedding_profile_id: None,
            limit: Some(5),
            page_size: Some(5),
            page: Some(1),
            fast: true,
            rerank: Some(false),
            dense_top_k: None,
            bm25_top_k: Some(5),
            rerank_top_n: None,
            bypass_cache: false,
            include_debug: true,
            include_debug_packs: false,
            include_locator: false,
            passage: false,
        }),
    )
    .await
    .unwrap()
    .0;
    (response, state)
}

fn assert_populated_bm25_startup_response(
    response: &RetrieveResponse,
    summary: &TaskSummaryResponse,
    expected_hits: usize,
) {
    assert_eq!(response.returned_results, expected_hits);
    assert!(response
        .results
        .iter()
        .all(|result| result.snippet.contains("startupneedle")));
    let debug = response.debug.as_ref().expect("retrieval debug");
    assert_eq!(debug.dense_vector_path, RetrievalDenseVectorPath::Bm25Only);
    assert_eq!(debug.query_embedding_latency_ms, None);
    assert_eq!(debug.local_spans_ms.query_embedding_ms, 0);
    assert_eq!(debug.local_spans_ms.dense_vector_search_ms, 0);
    let encoded_debug = serde_json::to_value(debug).unwrap();
    assert!(encoded_debug["local_spans_ms"]["bm25_search_ms"].is_u64());
    assert!(encoded_debug["local_spans_ms"]["response_formatting_ms"].is_u64());
    assert!(debug.dense_hits.is_empty());
    assert_eq!(debug.bm25_hits.len(), expected_hits);
    let count = debug
        .retrieval_search_sql_statement_count
        .expect("retrieval search statement count");
    assert!(count > 0);
    let retrieval_span = summary
        .spans
        .iter()
        .find(|span| span.phase == "retrieval")
        .expect("durable retrieval span");
    assert_eq!(
        retrieval_span.metadata["retrieval_search_sql_statement_count"],
        serde_json::json!(count)
    );
    let telemetry = serde_json::to_string(&retrieval_span.metadata).unwrap();
    for forbidden in [
        "SELECT",
        "startupneedle",
        "startup-src-alpha",
        "startup-chunk-alpha-1",
        "/populated-db-retrieve-bm25-startup",
    ] {
        assert!(!telemetry.contains(forbidden));
    }
}

#[tokio::test]
async fn ask_with_bm25_only_retrieval_uses_configured_chat_without_embedding_calls() {
    let model_server = MockModelServer::start_with_chat(3, "BM25 answer from evidence [E1]").await;
    let test_dir = TestDir::new("ask-bm25-only-chat");
    let source_path = test_dir.path().join("doc.md");
    fs::write(
        &source_path,
        "Alpha BM25-only evidence answers the generated ask question.",
    )
    .unwrap();
    let mut config = retrieve_test_config(&model_server.base_url);
    config.embedding.enabled = false;
    config.chat.enabled = true;
    config.chat.base_url = model_server.base_url.clone();
    config.chat.model = "test-chat".into();
    config.rerank.enabled = false;

    let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let source_id = pipeline.add_source(&source_path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();

    let state = test_state(config, test_dir.path(), pipeline);
    let hidden_response = ask(
        State(Arc::clone(&state)),
        Json(AskRequest {
            question: "What does Alpha evidence answer?".into(),
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

    assert!(hidden_response.answer.contains("BM25 answer"));
    assert!(hidden_response.retrieval.is_none());
    let hidden_task_id = latest_task_id(&state, TaskKind::Ask);
    let hidden_summary = task_summary_response(&state, hidden_task_id).await.unwrap();
    let hidden_retrieval_span = hidden_summary
        .spans
        .iter()
        .find(|span| span.phase == "retrieval")
        .expect("durable retrieval span");
    assert!(hidden_retrieval_span.metadata["retrieval_search_sql_statement_count"].is_u64());

    let exposed_response = ask(
        State(Arc::clone(&state)),
        Json(AskRequest {
            question: "What does Alpha evidence answer?".into(),
            source_id: Some(source_id.0),
            collection_filter: CollectionFilterRequest::default(),
            embedding_profile_id: None,
            show_retrieval: true,
            context_only: false,
            limit: None,
            page_size: None,
            page: None,
        }),
    )
    .await
    .unwrap()
    .0;
    let exposed_count = exposed_response
        .retrieval
        .as_ref()
        .expect("exposed retrieval debug")
        .retrieval_search_sql_statement_count
        .expect("retrieval search statement count");
    assert!(exposed_count > 0);
    let exposed_task_id = latest_task_id(&state, TaskKind::Ask);
    let exposed_summary = task_summary_response(&state, exposed_task_id)
        .await
        .unwrap();
    let exposed_retrieval_span = exposed_summary
        .spans
        .iter()
        .find(|span| span.phase == "retrieval")
        .expect("durable retrieval span");
    assert_eq!(
        exposed_retrieval_span.metadata["retrieval_search_sql_statement_count"],
        serde_json::json!(exposed_count)
    );
    assert_eq!(model_server.embedding_requests(), 0);
    assert_eq!(model_server.chat_requests(), 2);
}
