use tempfile::tempdir;

#[tokio::test]
async fn scoped_retrieval_keeps_configured_candidate_limits() {
    let store = Store::in_memory().unwrap();
    let first = source("src-1");
    let second = source("src-2");
    let alpha = insert_child(&store, &first, "chunk-alpha", "alpha content");
    let beta = insert_child(&store, &second, "chunk-beta", "beta content");
    store
        .replace_all_vector_documents(&[
            VectorDocument {
                chunk_id: alpha.id.clone(),
                source_id: first.id.clone(),
                vector: keyword_vector(&alpha.text),
            },
            VectorDocument {
                chunk_id: beta.id.clone(),
                source_id: second.id.clone(),
                vector: keyword_vector(&beta.text),
            },
        ])
        .unwrap();
    let mut hnsw = HnswIndex::new();
    hnsw.rebuild_from_store(&store).unwrap();
    let lexical_index = SqliteFtsIndex::new(&store);
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig {
        dense_top_k: 1,
        bm25_top_k: 1,
        ..RetrievalConfig::default()
    };
    let pipeline = RetrievalPipeline::new(
        &hnsw,
        &lexical_index,
        &store,
        &embed_client,
        &config,
    );

    let (results, debug) = pipeline
        .search_filtered_with_debug("beta", Some(&second.id))
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk_id.0, "chunk-beta");
    assert_eq!(results[0].chunk.source_id, second.id);
    assert_eq!(results[0].evidence_units.len(), 1);
    assert_eq!(
        debug
            .candidate_counters
            .requested_k(SpanKind::DenseRetrieval),
        1
    );
    assert_eq!(
        debug
            .candidate_counters
            .requested_k(SpanKind::LexicalRetrieval),
        1
    );
    assert_eq!(
        debug
            .candidate_counters
            .returned_k(SpanKind::DenseRetrieval),
        1
    );
    assert_eq!(
        debug
            .candidate_counters
            .returned_k(SpanKind::LexicalRetrieval),
        1
    );
    assert_eq!(debug.candidate_counters.evaluated(), 2);
    assert_eq!(debug.candidate_counters.filtered(), 0);
    assert_eq!(debug.candidate_counters.fused(), 1);
    assert_eq!(debug.candidate_counters.reranked(), 0);
    assert_eq!(debug.candidate_counters.hydrated(), 1);
}

#[tokio::test]
async fn source_filter_adaptively_overfetches_local_dense_top_k() {
    let store = Store::in_memory().unwrap();
    let wanted = source("src-wanted");
    let other = source("src-other");
    let trailing = source("src-trailing");
    let wanted_chunk = insert_child(&store, &wanted, "chunk-wanted", "wanted content");
    let other_chunk = insert_child(&store, &other, "chunk-other", "other content");
    let trailing_chunk = insert_child(
        &store,
        &trailing,
        "chunk-trailing",
        "trailing content",
    );
    let vector_index = StaticVectorIndex::new(vec![
        (other_chunk.id, 1.0),
        (wanted_chunk.id.clone(), 0.5),
        (trailing_chunk.id, 0.25),
    ]);
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig {
        dense_top_k: 1,
        bm25_top_k: 0,
        ..RetrievalConfig::default()
    };
    let pipeline = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &config,
    );

    let (results, debug) = pipeline
        .search_filtered_with_debug("wanted", Some(&wanted.id))
        .await
        .unwrap();

    assert_eq!(results.len(), 1);
    assert_eq!(results[0].chunk_id, wanted_chunk.id);
    assert_eq!(
        debug
            .candidate_counters
            .requested_k(SpanKind::DenseRetrieval),
        1
    );
}

#[cfg(feature = "qdrant")]
#[test]
fn adaptive_scoped_validation_batches_small_and_cap_sized_foreign_prefixes() {
    const CAP_SIZED_FOREIGN_PREFIX: usize = 256;

    let directory = tempdir().expect("temporary adaptive retrieval database");
    let database_path = directory.path().join("adaptive-validation.db");
    let writer = Store::new(&database_path).expect("writable adaptive retrieval store");
    let wanted = source("source-adaptive-wanted");
    let foreign = source("source-adaptive-foreign");
    writer.add_source(&wanted).expect("wanted source");
    writer.add_source(&foreign).expect("foreign source");
    let foreign_ids = (0..CAP_SIZED_FOREIGN_PREFIX)
        .map(|rank| {
            insert_text_chunk(
                &writer,
                &foreign,
                &format!("chunk-foreign-{rank:03}"),
                &format!("foreign content {rank:03}"),
            )
            .id
        })
        .collect::<Vec<_>>();
    let wanted_chunk = insert_text_chunk(
        &writer,
        &wanted,
        "chunk-adaptive-wanted",
        "wanted content",
    );
    drop(writer);

    let store = Store::open_existing_readonly(&database_path)
        .expect("readonly adaptive retrieval store");
    let small_hits = vec![
        (foreign_ids[0].clone(), 1.0),
        (wanted_chunk.id.clone(), 0.5),
        (foreign_ids[1].clone(), 0.25),
    ];
    let mut cap_hits = foreign_ids
        .iter()
        .enumerate()
        .map(|(rank, id)| {
            (
                id.clone(),
                (CAP_SIZED_FOREIGN_PREFIX - rank) as f32,
            )
        })
        .collect::<Vec<_>>();
    cap_hits.push((wanted_chunk.id.clone(), 0.0));
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig::default();
    let source_filter = HashSet::from([wanted.id]);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let measure = |hits| {
        let (qdrant_url, handle) =
            spawn_qdrant_search_response(200, r#"{"status":"ok","result":[]}"#);
        let vector_index = StaticVectorIndex::new(hits);
        let pipeline = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &config,
        )
        .with_qdrant_search(&qdrant_config(qdrant_url));
        let mut counters = CandidateCounters::default();
        let mut spans = RetrievalLocalSpansMs::default();
        let measured = store.count_sql_statements(|| {
            runtime.block_on(pipeline.dense_search(
                &[1.0, 0.0],
                1,
                Some(&source_filter),
                &mut counters,
                &mut spans,
            ))
        });
        assert_eq!(
            handle.join().unwrap(),
            "POST /collections/verbatim/points/search HTTP/1.1"
        );
        measured
    };
    let (small_result, small_count) = measure(small_hits);
    let (cap_result, cap_count) = measure(cap_hits);

    assert_eq!(
        small_result.expect("small adaptive search succeeds").0,
        vec![(wanted_chunk.id, 0.5)]
    );
    assert_eq!(
        cap_result
            .expect_err("cap-sized foreign prefix exhausts strict filtering")
            .downcast_ref::<crate::overfetch::OverfetchError>(),
        Some(&crate::overfetch::OverfetchError::UnsupportedStrictFilter)
    );
    assert_eq!(small_count, Some(5));
    assert_eq!(cap_count, Some(19));
}

#[tokio::test]
async fn adaptive_source_filter_keeps_valid_hit_when_prior_peer_is_corrupt() {
    let store = Store::in_memory().unwrap();
    let wanted = source("src-adaptive-corrupt-peer");
    store.add_source(&wanted).unwrap();
    let corrupt = insert_text_chunk(&store, &wanted, "chunk-corrupt", "corrupt content");
    let valid = insert_text_chunk(&store, &wanted, "chunk-valid", "valid content");
    corrupt_chunk_evidence_link(&store, &corrupt.id);
    let vector_index = StaticVectorIndex::new(vec![
        (corrupt.id, 1.0),
        (valid.id.clone(), 0.5),
        (ChunkId("chunk-trailing".into()), 0.25),
    ]);
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig {
        dense_top_k: 1,
        bm25_top_k: 0,
        ..RetrievalConfig::default()
    };
    let pipeline = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &config,
    );

    let results = pipeline
        .search_filtered("valid", Some(&wanted.id))
        .await
        .expect("adaptive source filtering isolates a corrupt candidate");

    assert_eq!(chunk_ids(&results), vec![valid.id.0]);
}

#[test]
fn scoped_search_setup_does_not_materialize_child_chunks() {
    assert!(!include_str!("../retrieve.rs").contains(&concat!("list_child_", "chunks()")));
}

fn corrupt_chunk_evidence_link(store: &Store, chunk_id: &ChunkId) {
    store
        .connection()
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .expect("allow malformed fixture link");
    store
        .connection()
        .execute(
            "UPDATE chunk_evidence SET evidence_unit_id = X'00' WHERE chunk_id = ?1",
            [&chunk_id.0],
        )
        .expect("corrupt candidate evidence link");
}

#[tokio::test]
async fn source_filter_keeps_valid_candidates_when_a_batch_peer_is_corrupt() {
    let store = Store::in_memory().unwrap();
    let wanted = source("src-wanted");
    store.add_source(&wanted).unwrap();
    let corrupt = insert_text_chunk(&store, &wanted, "chunk-corrupt", "corrupt content");
    let valid = insert_text_chunk(&store, &wanted, "chunk-valid", "valid content");
    corrupt_chunk_evidence_link(&store, &corrupt.id);

    let vector_index = StaticVectorIndex::new(vec![
        (corrupt.id, 1.0),
        (ChunkId("chunk-missing".into()), 0.75),
        (valid.id.clone(), 0.5),
    ]);
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig {
        dense_top_k: 3,
        bm25_top_k: 0,
        ..RetrievalConfig::default()
    };
    let pipeline = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &config,
    );

    let results = pipeline
        .search_filtered("valid", Some(&wanted.id))
        .await
        .expect("source-filtered retrieval succeeds");

    assert_eq!(chunk_ids(&results), vec![valid.id.0]);
}

#[tokio::test]
async fn final_hydration_keeps_valid_candidates_when_a_batch_peer_is_corrupt() {
    let store = Store::in_memory().unwrap();
    let wanted = source("src-wanted");
    store.add_source(&wanted).unwrap();
    let corrupt = insert_text_chunk(&store, &wanted, "chunk-corrupt", "corrupt content");
    let valid = insert_text_chunk(&store, &wanted, "chunk-valid", "valid content");
    corrupt_chunk_evidence_link(&store, &corrupt.id);

    let vector_index = StaticVectorIndex::new(vec![
        (corrupt.id, 1.0),
        (ChunkId("chunk-missing".into()), 0.75),
        (valid.id.clone(), 0.5),
    ]);
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig {
        dense_top_k: 3,
        bm25_top_k: 0,
        ..RetrievalConfig::default()
    };
    let pipeline = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &config,
    );

    let results = pipeline
        .search_filtered("valid", None)
        .await
        .expect("unfiltered retrieval succeeds");

    assert_eq!(chunk_ids(&results), vec![valid.id.0]);
}

#[test]
fn batch_chunk_loading_isolates_bad_links_but_propagates_query_failures() {
    let store = Store::in_memory().unwrap();
    let wanted = source("src-wanted");
    store.add_source(&wanted).unwrap();
    let corrupt = insert_text_chunk(&store, &wanted, "chunk-corrupt", "corrupt content");
    let valid = insert_text_chunk(&store, &wanted, "chunk-valid", "valid content");
    corrupt_chunk_evidence_link(&store, &corrupt.id);

    let chunks = store
        .get_chunks(&[corrupt.id.clone(), valid.id.clone()])
        .expect("batch query succeeds");
    assert!(chunks
        .get(&corrupt.id)
        .expect("corrupt candidate is represented")
        .is_err());
    assert_eq!(
        chunks
            .get(&valid.id)
            .expect("valid candidate is represented")
            .as_ref()
            .expect("valid candidate loads")
            .id,
        valid.id
    );

    let unavailable_store = Store::in_memory().unwrap();
    unavailable_store
        .connection()
        .execute_batch("DROP TABLE chunks;")
        .expect("remove chunks table");
    assert!(unavailable_store
        .get_chunks(&[ChunkId("chunk-valid".into())])
        .is_err());
}

#[tokio::test]
async fn source_filter_propagates_batch_query_failures() {
    let store = Store::in_memory().unwrap();
    store
        .connection()
        .execute_batch("DROP TABLE chunks;")
        .expect("remove chunks table");
    let wanted = source("src-wanted");
    let vector_index = StaticVectorIndex::new(vec![(ChunkId("chunk-missing".into()), 1.0)]);
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig {
        dense_top_k: 1,
        bm25_top_k: 0,
        ..RetrievalConfig::default()
    };
    let pipeline = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &config,
    );

    assert!(pipeline
        .search_filtered("missing", Some(&wanted.id))
        .await
        .is_err());
}

#[test]
fn batch_hydration_keeps_sql_statement_count_constant_across_fused_candidate_pools() {
    const FINAL_RESULT_COUNT: usize = 10;
    const LARGE_FUSED_CANDIDATE_POOL: usize = 10_000;

    fn batch_fixture_chunk(
        source: &Source,
        id: String,
        text: String,
        parent_chunk_id: Option<ChunkId>,
        chunk_type: ChunkType,
    ) -> (Chunk, EvidenceUnit) {
        let evidence = EvidenceUnit {
            id: EvidenceId(format!("evidence-{id}")),
            source_id: source.id.clone(),
            kind: EvidenceKind::Text,
            derived_from: None,
            locator: SourceLocator::Document {
                path_or_url: source.path.to_string_lossy().into_owned(),
                line_start: 1,
                line_end: None,
            },
            text: text.clone(),
            text_hash: format!("hash-evidence-{id}"),
            heading_path: vec!["batch".into()],
            position: 0,
        };
        let chunk = Chunk {
            id: ChunkId(id.clone()),
            source_id: source.id.clone(),
            chunk_hash: format!("hash-{id}"),
            embedding_input_hash: None,
            text,
            context_text: None,
            token_count: 4,
            chunk_type,
            parent_chunk_id,
            heading_path: vec!["batch".into()],
            evidence_unit_ids: vec![evidence.id.clone()],
        };
        (chunk, evidence)
    }

    let directory = tempdir().expect("temporary retrieval database");
    let database_path = directory.path().join("retrieval.db");
    let writer = Store::new(&database_path).expect("writable retrieval store");
    let wanted = source("source-wanted");
    let excluded = source("source-excluded");
    writer.add_source(&wanted).expect("wanted source");
    writer.add_source(&excluded).expect("excluded source");

    let mut chunks = Vec::with_capacity(LARGE_FUSED_CANDIDATE_POOL + FINAL_RESULT_COUNT);
    let mut evidence = Vec::with_capacity(LARGE_FUSED_CANDIDATE_POOL + FINAL_RESULT_COUNT);
    let mut links = Vec::with_capacity(LARGE_FUSED_CANDIDATE_POOL + FINAL_RESULT_COUNT);
    let mut small_hits = Vec::with_capacity(FINAL_RESULT_COUNT);
    let mut large_hits = Vec::with_capacity(LARGE_FUSED_CANDIDATE_POOL);

    for index in 0..FINAL_RESULT_COUNT {
        let parent_id = ChunkId(format!("parent-{index:05}"));
        let (parent, parent_evidence) = batch_fixture_chunk(
            &wanted,
            parent_id.0.clone(),
            format!("parent result {index}"),
            None,
            ChunkType::Parent,
        );
        links.push((parent.id.clone(), parent_evidence.id.clone()));
        chunks.push(parent);
        evidence.push(parent_evidence);

        let (child, child_evidence) = batch_fixture_chunk(
            &wanted,
            format!("wanted-{index:05}"),
            format!("wanted result {index}"),
            Some(parent_id),
            ChunkType::Child,
        );
        let hit = (child.id.clone(), 1.0 - index as f32 / FINAL_RESULT_COUNT as f32);
        links.push((child.id.clone(), child_evidence.id.clone()));
        chunks.push(child);
        evidence.push(child_evidence);
        small_hits.push(hit.clone());
        large_hits.push(hit);
    }

    for index in FINAL_RESULT_COUNT..LARGE_FUSED_CANDIDATE_POOL {
        let (chunk, unit) = batch_fixture_chunk(
            &excluded,
            format!("excluded-{index:05}"),
            format!("excluded result {index}"),
            None,
            ChunkType::Child,
        );
        large_hits.push((chunk.id.clone(), 0.5 - index as f32 / LARGE_FUSED_CANDIDATE_POOL as f32));
        links.push((chunk.id.clone(), unit.id.clone()));
        chunks.push(chunk);
        evidence.push(unit);
    }
    writer
        .bulk_insert_evidence(&evidence)
        .expect("fixture evidence");
    writer.bulk_insert_chunks(&chunks).expect("fixture chunks");
    writer.link_chunk_evidence(&links).expect("fixture links");
    drop(writer);

    let store = Store::open_existing_readonly(&database_path).expect("readonly retrieval store");
    let config = RetrievalConfig {
        dense_top_k: LARGE_FUSED_CANDIDATE_POOL,
        bm25_top_k: 0,
        ..RetrievalConfig::default()
    };
    let embed_client = KeywordEmbeddingClient;
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");

    let small_vector_index = StaticVectorIndex::new(small_hits);
    let small_lexical_index = StaticLexicalIndex::new(Vec::new());
    let small_pipeline = RetrievalPipeline::new(
        &small_vector_index,
        &small_lexical_index,
        &store,
        &embed_client,
        &config,
    );
    let (small_result, small_count) = store.count_sql_statements(|| {
        runtime.block_on(small_pipeline.search_filtered("batch", Some(&wanted.id)))
    });
    let small_results = small_result.expect("small retrieval succeeds");

    let large_vector_index = StaticVectorIndex::new(large_hits);
    let large_lexical_index = StaticLexicalIndex::new(Vec::new());
    let large_pipeline = RetrievalPipeline::new(
        &large_vector_index,
        &large_lexical_index,
        &store,
        &embed_client,
        &config,
    );
    let (large_result, large_count) = store.count_sql_statements(|| {
        runtime.block_on(large_pipeline.search_filtered("batch", Some(&wanted.id)))
    });
    let large_results = large_result.expect("large retrieval succeeds");

    assert_eq!(small_results.len(), FINAL_RESULT_COUNT);
    assert_eq!(
        chunk_ids(&small_results),
        chunk_ids(&large_results),
        "source filtering keeps the bounded final result set stable"
    );
    assert!(large_results
        .iter()
        .all(|result| result.chunk.chunk_type == ChunkType::Parent));
    assert!(large_results
        .iter()
        .all(|result| result.evidence_units.len() == 1));
    assert_eq!(
        small_count.expect("small readonly statement count"),
        large_count.expect("large readonly statement count"),
        "bounded final results must not make SQL grow with the fused candidate pool"
    );
}

#[test]
fn rerank_candidate_hydration_keeps_sql_statement_count_constant_across_candidate_pools() {
    const SMALL_CANDIDATE_POOL: usize = 10;
    const LARGE_CANDIDATE_POOL: usize = MAX_RERANK_CANDIDATE_CHUNKS;

    let directory = tempdir().expect("temporary rerank database");
    let database_path = directory.path().join("rerank.db");
    let writer = Store::new(&database_path).expect("writable rerank store");
    let source = source("source-rerank");
    writer.add_source(&source).expect("rerank source");
    let candidate_ids = (0..LARGE_CANDIDATE_POOL)
        .map(|index| {
            insert_text_chunk(
                &writer,
                &source,
                &format!("candidate-{index:02}"),
                &format!("candidate text {index:02}"),
            )
            .id
        })
        .collect::<Vec<_>>();
    drop(writer);

    let store = Store::open_existing_readonly(&database_path).expect("readonly rerank store");
    let vector_index = StaticVectorIndex::new(Vec::new());
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let retrieval_config = RetrievalConfig::default();
    let rerank_config = RerankConfig {
        enabled: true,
        allow_document_export: false,
        base_url: "http://localhost:8003/v1".into(),
        top_n: 2,
        ..Default::default()
    };
    let reranker = RecordingReranker::hits(vec![(9, 1.0), (0, 0.9)]);
    let pipeline = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &retrieval_config,
    )
    .with_reranker(&rerank_config, &reranker);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    let fused = candidate_ids
        .iter()
        .enumerate()
        .map(|(index, id)| (id.clone(), 1.0 - index as f32 / LARGE_CANDIDATE_POOL as f32))
        .collect::<Vec<_>>();

    let (small_result, small_count) = store.count_sql_statements(|| {
        runtime.block_on(pipeline.rerank_fused(
            "candidate",
            fused[..SMALL_CANDIDATE_POOL].to_vec(),
        ))
    });
    let (large_result, large_count) = store.count_sql_statements(|| {
        runtime.block_on(pipeline.rerank_fused("candidate", fused.clone()))
    });
    let small = small_result.expect("small rerank succeeds");
    let large = large_result.expect("large rerank succeeds");

    assert_eq!(small.fused, large.fused, "rerank selection stays stable");
    let documents = reranker.recorded_docs();
    assert_eq!(documents.len(), 2);
    assert_eq!(documents[0], documents[1][..SMALL_CANDIDATE_POOL]);
    assert_eq!(documents[1].len(), LARGE_CANDIDATE_POOL);
    let small_count = small_count.expect("small readonly statement count");
    let large_count = large_count.expect("large readonly statement count");
    assert_eq!(small_count, large_count);
    assert_eq!(
        small_count, 2,
        "rerank hydration must use exactly one chunk batch read"
    );
}

#[tokio::test]
async fn rerank_duplicate_candidate_ids_preserve_document_positions() {
    let store = Store::in_memory().expect("rerank store");
    let source = source("source-rerank-duplicates");
    store.add_source(&source).expect("rerank source");
    let first = insert_text_chunk(&store, &source, "candidate-a", "candidate text A");
    let second = insert_text_chunk(&store, &source, "candidate-b", "candidate text B");
    let vector_index = StaticVectorIndex::new(Vec::new());
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let retrieval_config = RetrievalConfig::default();
    let rerank_config = RerankConfig {
        enabled: true,
        allow_document_export: false,
        base_url: "http://localhost:8003/v1".into(),
        top_n: 2,
        ..Default::default()
    };
    let reranker = RecordingReranker::hits(vec![(1, 1.0), (2, 0.9)]);
    let outcome = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &retrieval_config,
    )
    .with_reranker(&rerank_config, &reranker)
    .rerank_fused(
        "candidate",
        vec![
            (first.id.clone(), 0.9),
            (first.id.clone(), 0.8),
            (second.id.clone(), 0.7),
        ],
    )
    .await
    .expect("duplicate rerank succeeds");

    assert_eq!(
        reranker.recorded_docs(),
        vec![vec![
            "candidate text A".to_string(),
            "candidate text A".to_string(),
            "candidate text B".to_string(),
        ]]
    );
    assert_eq!(outcome.fused, vec![(first.id, 1.0), (second.id, 0.9)]);
}
