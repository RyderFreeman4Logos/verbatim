use super::*;

fn insert_evidence_kind_chunk(
    store: &Store,
    source: &Source,
    chunk_id: &str,
    text: &str,
    kind: EvidenceKind,
) -> Chunk {
    let evidence = EvidenceUnit {
        id: EvidenceId(format!("ev-{chunk_id}")),
        source_id: source.id.clone(),
        kind,
        derived_from: None,
        locator: SourceLocator::Document {
            path_or_url: source.path.to_string_lossy().into_owned(),
            line_start: 1,
            line_end: None,
        },
        text: text.into(),
        text_hash: format!("hash-{chunk_id}"),
        heading_path: Vec::new(),
        language: None,
        position: 0,
    };
    let chunk = Chunk {
        id: ChunkId(chunk_id.into()),
        source_id: source.id.clone(),
        chunk_hash: format!("hash-{chunk_id}"),
        embedding_input_hash: None,
        text: text.into(),
        context_text: None,
        token_count: 4,
        chunk_type: ChunkType::Child,
        parent_chunk_id: None,
        heading_path: Vec::new(),
        evidence_unit_ids: vec![evidence.id.clone()],
    };
    store.bulk_insert_evidence(&[evidence]).unwrap();
    store
        .bulk_insert_chunks(std::slice::from_ref(&chunk))
        .unwrap();
    store
        .link_chunk_evidence(&[(chunk.id.clone(), chunk.evidence_unit_ids[0].clone())])
        .unwrap();
    chunk
}

#[tokio::test]
async fn endpoint_rerank_filters_derived_candidates_before_budgeting() {
    assert_derived_candidates_filtered_before_budgeting(RerankStrategy::Endpoint).await;
}

#[tokio::test]
async fn llm_rerank_filters_derived_candidates_before_budgeting() {
    assert_derived_candidates_filtered_before_budgeting(RerankStrategy::Llm).await;
}

async fn assert_derived_candidates_filtered_before_budgeting(strategy: RerankStrategy) {
    let store = Store::in_memory().unwrap();
    let source = source("src-rerank-source-boundary");
    store.add_source(&source).unwrap();
    let ocr = insert_evidence_kind_chunk(
        &store,
        &source,
        "chunk-ocr",
        "OCR candidate must never reach rerank",
        EvidenceKind::Ocr,
    );
    let generated = insert_evidence_kind_chunk(
        &store,
        &source,
        "chunk-generated",
        "Generated candidate must never reach rerank",
        EvidenceKind::Generated,
    );
    let source_chunk = insert_text_chunk(
        &store,
        &source,
        "chunk-source",
        "Source-backed candidate remains available",
    );
    let vector_index = StaticVectorIndex::new(vec![
        (ocr.id.clone(), 0.99),
        (generated.id.clone(), 0.98),
        (source_chunk.id.clone(), 0.97),
    ]);
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let retrieval_config = RetrievalConfig {
        dense_top_k: 3,
        bm25_top_k: 0,
        rrf_k: 60,
        default_limit: 1,
        ..RetrievalConfig::default()
    };
    let rerank_config = RerankConfig {
        enabled: true,
        strategy,
        top_n: 2,
        ..Default::default()
    };
    let reranker = RecordingReranker::hits(vec![(0, 0.99), (1, 0.98)]);

    let (results, _debug) = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &retrieval_config,
    )
    .with_reranker(&rerank_config, &reranker)
    .search_filtered_with_debug("candidate", None)
    .await
    .unwrap();

    assert_eq!(
        reranker.recorded_docs(),
        vec![vec!["Source-backed candidate remains available".to_string()]]
    );
    assert_eq!(chunk_ids(&results), vec!["chunk-source"]);
}
