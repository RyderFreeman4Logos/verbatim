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
