#[test]
fn ask_debug_selection_stays_scoped_when_canonical_candidates_grow() {
    use verbatim_core::traits::LexicalIndex;

    let test_dir = TestDir::new("ask-debug-selection-budget");
    let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
    config.embedding.enabled = false;
    config.rerank.enabled = false;
    config.retrieval.default_limit = 2;
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let store = pipeline.store();
    let source = verbatim_core::types::Source {
        id: SourceId("ask-debug-selection-budget".into()),
        path: test_dir.path().join("source.jsonl"),
        hash: "ask-debug-selection-budget".into(),
        status: SourceStatus::Indexed,
        parser_used: Some("canonical_jsonl".into()),
        last_ingested_at: None,
    };
    store.add_source(&source).unwrap();
    let canonical_result = |rank, chunk_id, prefix| {
        let mut result = test_canonical_retrieval_result(
            rank,
            chunk_id,
            &[
                (format!("{prefix}-opening").as_str(), rank as u32),
                (format!("{prefix}-support").as_str(), rank as u32 + 10),
            ],
        );
        result.chunk.source_id = source.id.clone();
        for evidence in &mut result.evidence_units {
            evidence.source_id = source.id.clone();
        }
        result.evidence_units[0].text = format!("{prefix} opening");
        result.evidence_units[1].text = format!("alpha {prefix} support");
        result.chunk.text = result
            .evidence_units
            .iter()
            .map(|evidence| evidence.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
        result
    };
    let first = canonical_result(1, "chunk-first", "first");
    let second = canonical_result(2, "chunk-second", "second");
    let tail = canonical_result(3, "chunk-tail", "tail");
    let insert_results = |results: &[RetrievalResult]| {
        let evidence = results
            .iter()
            .flat_map(|result| result.evidence_units.clone())
            .collect::<Vec<_>>();
        let chunks = results
            .iter()
            .map(|result| result.chunk.clone())
            .collect::<Vec<_>>();
        let links = chunks
            .iter()
            .flat_map(|chunk| {
                chunk
                    .evidence_unit_ids
                    .iter()
                    .cloned()
                    .map(|evidence_id| (chunk.id.clone(), evidence_id))
            })
            .collect::<Vec<_>>();
        store.bulk_insert_evidence(&evidence).unwrap();
        store.bulk_insert_chunks(&chunks).unwrap();
        store.link_chunk_evidence(&links).unwrap();
    };
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let run_ask_retrieval = || {
        let lexical_index = pipeline.lexical_index();
        lexical_index.rebuild_from_store(store).unwrap();
        let embed_client = OpenAiEmbeddingClient::new(&config.embedding);
        run_generation_retrieval(
            runtime.handle().clone(),
            RetrievalPipeline::new(
                pipeline.vector_index(),
                &lexical_index,
                store,
                &embed_client,
                &config.retrieval,
            )
            .with_embedding_enabled(false),
            &config,
            "alpha",
            None,
            true,
        )
        .unwrap()
    };

    insert_results(&[first, second]);
    let (small_results, small_debug) = run_ask_retrieval();
    let small_order = small_results
        .iter()
        .map(|result| result.chunk_id.0.as_str())
        .collect::<Vec<_>>();
    assert_eq!(
        small_debug
            .unwrap()
            .display_evidence_pack
            .iter()
            .map(|entry| entry.evidence_id.0.as_str())
            .collect::<Vec<_>>(),
        vec!["first-support", "second-support"]
    );

    insert_results(std::slice::from_ref(&tail));
    let (large_results, large_debug) = run_ask_retrieval();
    assert_eq!(
        large_results
            .iter()
            .take(small_order.len())
            .map(|result| result.chunk_id.0.as_str())
            .collect::<Vec<_>>(),
        small_order
    );
    assert_eq!(
        large_debug
            .unwrap()
            .display_evidence_pack
            .iter()
            .map(|entry| entry.evidence_id.0.as_str())
            .collect::<Vec<_>>(),
        vec!["first-support", "second-support", "tail-opening"]
    );
}
