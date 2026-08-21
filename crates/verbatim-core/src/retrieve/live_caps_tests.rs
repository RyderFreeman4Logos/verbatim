use super::*;

#[tokio::test]
async fn live_retrieval_caps_fused_pool_before_hydration() {
    let store = Store::in_memory().unwrap();
    let source = source("src-live-fused-cap");
    store.add_source(&source).unwrap();
    let chunks = (0..6)
        .map(|index| {
            TextChunkFixture::new(
                &store,
                &source,
                &format!("chunk-{index}"),
                &format!("candidate {index}"),
            )
            .insert()
        })
        .collect::<Vec<_>>();
    let vector_hits = chunks[..4]
        .iter()
        .enumerate()
        .map(|(index, chunk)| (chunk.id.clone(), 1.0 - index as f32 / 10.0))
        .collect::<Vec<_>>();
    let lexical_hits = chunks[2..]
        .iter()
        .enumerate()
        .map(|(index, chunk)| (chunk.id.clone(), 1.0 - index as f32 / 10.0))
        .collect::<Vec<_>>();
    let vector_index = StaticVectorIndex::new(vector_hits);
    let lexical_index = StaticLexicalIndex::new(lexical_hits);
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig {
        dense_top_k: 4,
        bm25_top_k: 4,
        default_limit: 4,
        ..RetrievalConfig::default()
    };

    let (results, debug) = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &config,
    )
    .search_source_set_with_debug("candidate", None)
    .await
    .unwrap();

    assert_eq!(debug.candidate_counters.fused(), 4);
    assert_eq!(debug.candidate_counters.hydrated(), 4);
    assert_eq!(results.len(), 4);
}

#[tokio::test]
async fn live_retrieval_caps_no_rerank_hydration_input() {
    let store = Store::in_memory().unwrap();
    let source = source("src-live-hydration-cap");
    store.add_source(&source).unwrap();
    let chunks = (0..6)
        .map(|index| {
            TextChunkFixture::new(
                &store,
                &source,
                &format!("chunk-{index}"),
                &format!("candidate {index}"),
            )
            .insert()
        })
        .collect::<Vec<_>>();
    let vector_index = StaticVectorIndex::new(
        chunks
            .iter()
            .enumerate()
            .map(|(index, chunk)| (chunk.id.clone(), 1.0 - index as f32 / 10.0))
            .collect(),
    );
    let lexical_index = StaticLexicalIndex::new(Vec::new());
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig {
        dense_top_k: 6,
        bm25_top_k: 0,
        default_limit: 2,
        ..RetrievalConfig::default()
    };

    let results = RetrievalPipeline::new(
        &vector_index,
        &lexical_index,
        &store,
        &embed_client,
        &config,
    )
    .search_source_set("candidate", None)
    .await
    .unwrap();

    assert_eq!(results.len(), 2);
}

#[test]
fn live_debug_hits_are_batched_and_capped() {
    let directory = tempfile::tempdir().unwrap();
    let database_path = directory.path().join("debug-caps.db");
    let writer = Store::new(&database_path).unwrap();
    let source = source("src-live-debug-cap");
    writer.add_source(&source).unwrap();
    let chunks = (0..4)
        .map(|index| {
            TextChunkFixture::new(
                &writer,
                &source,
                &format!("chunk-{index}"),
                &format!("candidate {index}"),
            )
            .insert()
        })
        .collect::<Vec<_>>();
    drop(writer);
    let store = Store::open_existing_readonly(&database_path).unwrap();
    let hits = chunks
        .iter()
        .enumerate()
        .map(|(index, chunk)| (chunk.id.clone(), 1.0 - index as f32 / 10.0))
        .collect::<Vec<_>>();
    let vector_index = StaticVectorIndex::new(hits.clone());
    let lexical_index = StaticLexicalIndex::new(hits);
    let embed_client = KeywordEmbeddingClient;
    let config = RetrievalConfig {
        dense_top_k: 4,
        bm25_top_k: 4,
        default_limit: 2,
        ..RetrievalConfig::default()
    };
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();

    for debug_options in [
        RetrievalDebugOptions::compact(RetrievalCanonicalSelectionBudget::all()),
        RetrievalDebugOptions::full(RetrievalCanonicalSelectionBudget::all()),
    ] {
        let pipeline = RetrievalPipeline::new(
            &vector_index,
            &lexical_index,
            &store,
            &embed_client,
            &config,
        );
        let (output, statement_count) = store.count_sql_statements(|| {
            runtime.block_on(pipeline.search_source_set_with_debug_options(
                "candidate",
                None,
                debug_options,
            ))
        });
        let (_, debug) = output.unwrap();
        assert_eq!(debug.dense_hits.len(), 2);
        assert_eq!(debug.bm25_hits.len(), 2);
        assert_eq!(debug.rrf_fused_hits.len(), 2);
        assert!(statement_count.unwrap() <= 16);
    }
}
