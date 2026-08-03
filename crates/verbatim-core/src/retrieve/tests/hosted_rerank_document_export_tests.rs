use super::*;

#[tokio::test]
async fn hosted_rerank_with_document_export_opt_in_replaces_rrf_order() {
    let store = Store::in_memory().unwrap();
    let source = source("src-rerank");
    store.add_source(&source).unwrap();
    let first = insert_text_chunk(&store, &source, "chunk-first", "alpha first");
    let second = insert_text_chunk(&store, &source, "chunk-second", "alpha second");
    let third = insert_text_chunk(&store, &source, "chunk-third", "alpha third");
    let vector_index = StaticVectorIndex::new(vec![
        (first.id.clone(), 0.9),
        (second.id.clone(), 0.8),
        (third.id.clone(), 0.7),
    ]);
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let retrieval_config = RetrievalConfig {
        dense_top_k: 3,
        bm25_top_k: 0,
        rrf_k: 60,
        ..RetrievalConfig::default()
    };
    let rerank_config = RerankConfig {
        enabled: true,
        allow_document_export: true,
        strategy: RerankStrategy::Endpoint,
        provider: "vllm".into(),
        base_url: "https://rerank.example.test/v1".into(),
        model: "test-reranker".into(),
        top_n: 2,
        ..Default::default()
    };
    let reranker = RecordingReranker::hits(vec![(2, 0.99), (0, 0.7)]);

    let (results, debug) = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &retrieval_config,
    )
    .with_reranker(&rerank_config, &reranker)
    .search_filtered_with_debug("alpha", None)
    .await
    .unwrap();

    assert_eq!(chunk_ids(&results), vec!["chunk-third", "chunk-first"]);
    assert_eq!(reranker.call_count(), 1);
    assert_eq!(reranker.recorded_top_ns(), vec![2]);
    assert_eq!(reranker.recorded_docs()[0].len(), 3);
    assert_eq!(debug.reranker.status, RetrievalRerankStatus::Succeeded);
    assert_eq!(debug.reranker.provider.as_deref(), Some("vllm"));
    assert_eq!(debug.reranker.model.as_deref(), Some("test-reranker"));
    assert_eq!(debug.reranker.top_n, Some(2));
    assert_eq!(debug.reranker.candidate_count, Some(3));
    assert_eq!(debug.reranker.scores[0].chunk_id, third.id);
    assert_eq!(debug.final_evidence_pack[0].chunk_id, third.id);
}

#[test]
fn rerank_endpoint_locality_fails_closed() {
    assert!(endpoint_is_local("http://127.0.0.1:8003"));
    assert!(endpoint_is_local("http://[::1]:8003/v1"));
    assert!(endpoint_is_local("http://LOCALHOST:8003"));
    assert!(!endpoint_is_local(""));
    assert!(!endpoint_is_local("not a URL"));
    assert!(!endpoint_is_local("https://rerank.example.test/v1"));
    assert!(!endpoint_is_local("http://localhost.attacker.example/v1"));
}

#[tokio::test]
async fn hosted_rerank_without_document_export_opt_in_preserves_rrf_order() {
    let store = Store::in_memory().unwrap();
    let source = source("src-rerank-hosted-blocked");
    store.add_source(&source).unwrap();
    let first = insert_text_chunk(&store, &source, "chunk-first", "alpha first");
    let second = insert_text_chunk(&store, &source, "chunk-second", "alpha second");
    let vector_index =
        StaticVectorIndex::new(vec![(first.id.clone(), 0.9), (second.id.clone(), 0.8)]);
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let retrieval_config = RetrievalConfig::default();
    let rerank_config = RerankConfig {
        enabled: true,
        base_url: "https://rerank.example.test/v1".into(),
        ..Default::default()
    };
    let reranker = RecordingReranker::hits(vec![(1, 1.0)]);

    let (results, debug) = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &retrieval_config,
    )
    .with_reranker(&rerank_config, &reranker)
    .search_filtered_with_debug("alpha", None)
    .await
    .unwrap();

    assert_eq!(chunk_ids(&results), vec!["chunk-first", "chunk-second"]);
    assert_eq!(reranker.call_count(), 0);
    assert_eq!(debug.reranker.status, RetrievalRerankStatus::Skipped);
    assert_eq!(
        debug.reranker.reason.as_deref(),
        Some("document_export_not_allowed")
    );
}
