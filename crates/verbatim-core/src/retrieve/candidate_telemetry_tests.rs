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
async fn source_filter_does_not_expand_local_dense_top_k() {
    let store = Store::in_memory().unwrap();
    let wanted = source("src-wanted");
    let other = source("src-other");
    let wanted_chunk = insert_child(&store, &wanted, "chunk-wanted", "wanted content");
    let other_chunk = insert_child(&store, &other, "chunk-other", "other content");
    let vector_index = StaticVectorIndex::new(vec![
        (other_chunk.id, 1.0),
        (wanted_chunk.id, 0.5),
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

    assert!(results.is_empty());
    assert_eq!(
        debug
            .candidate_counters
            .requested_k(SpanKind::DenseRetrieval),
        1
    );
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
