use super::*;
use verbatim_core::traits::LexicalIndex;

#[test]
fn retrieve_page_stops_at_hydrated_snapshot_without_following_ask_tail() {
    let (store, results, debug, controls) = retrieve_sidecar_fixture(false, 3);
    let sources = sources_for_results(&results, &store).unwrap();
    let response = retrieve_response(
        &store,
        RetrieveResponseInput {
            task_id: TaskId("task-retrieve-sidecar-page".into()),
            query: "alpha".into(),
            source_filter: None,
            collection_filter: None,
            collection_provenance: HashMap::new(),
            embedding_profile_id: EmbeddingProfileId::default_profile(),
            controls,
            results,
            debug,
            sources,
            retrieval_ms: 1,
        },
    )
    .expect("retrieve page must stay within source-bounded results");

    assert_eq!(response.total_results, 2);
    assert_eq!(response.returned_results, 0);
    assert!(response.results.is_empty());
}

#[test]
fn retrieve_debug_packs_exclude_generated_fused_tail_in_compact_and_full_modes() {
    for include_debug_packs in [false, true] {
        let (store, results, debug, controls) = retrieve_sidecar_fixture(include_debug_packs, 1);
        let serialized = serde_json::to_string(&debug).unwrap();
        assert!(!serialized.contains("chunk-tail-generated"));
        assert!(!serialized.contains("tail-generated-caption"));

        let sources = sources_for_results(&results, &store).unwrap();
        let response = retrieve_response(
            &store,
            RetrieveResponseInput {
                task_id: TaskId("task-retrieve-sidecar-sanitizer".into()),
                query: "alpha".into(),
                source_filter: None,
                collection_filter: None,
                collection_provenance: HashMap::new(),
                embedding_profile_id: EmbeddingProfileId::default_profile(),
                controls,
                results,
                debug,
                sources,
                retrieval_ms: 1,
            },
        )
        .unwrap();
        let response_json = serde_json::to_string(&response).unwrap();
        assert!(!response_json.contains("chunk-tail-generated"));
        assert!(!response_json.contains("tail-generated-caption"));
    }
}

fn retrieve_sidecar_fixture(
    include_debug_packs: bool,
    page: usize,
) -> (
    Store,
    Vec<RetrievalResult>,
    RetrievalDebug,
    EffectiveRetrieveControls,
) {
    let test_dir = TestDir::new("retrieve-sidecar-isolation");
    let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
    config.embedding.enabled = false;
    config.rerank.enabled = false;
    config.retrieval.default_limit = 2;
    config.retrieval.bm25_top_k = 4;
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let store = pipeline.store();
    let source = Source {
        id: SourceId("src".into()),
        path: test_dir.path().join("source.jsonl"),
        hash: "retrieve-sidecar-isolation".into(),
        status: SourceStatus::Indexed,
        parser_used: Some("canonical_jsonl".into()),
        last_ingested_at: None,
    };
    store.add_source(&source).unwrap();

    let mut first = test_canonical_retrieval_result(
        1,
        "chunk-first",
        &[("first-opening", 1), ("first-support", 2)],
    );
    let mut second = test_canonical_retrieval_result(
        2,
        "chunk-second",
        &[("second-opening", 3), ("second-support", 4)],
    );
    let mut tail = test_retrieval_result(
        3,
        "chunk-tail-generated",
        "tail-generated-caption",
        EvidenceKind::Generated,
    );
    for result in [&mut first, &mut second] {
        result.chunk.source_id = source.id.clone();
        for evidence in &mut result.evidence_units {
            evidence.source_id = source.id.clone();
            evidence.text = format!("alpha {}", evidence.id.0);
            evidence.text_hash = verbatim_core::types::hex_sha256(evidence.text.as_bytes());
        }
        result.chunk.text = result
            .evidence_units
            .iter()
            .map(|evidence| evidence.text.as_str())
            .collect::<Vec<_>>()
            .join(" ");
    }
    tail.chunk.source_id = source.id.clone();
    tail.evidence_units[0].source_id = source.id.clone();
    tail.evidence_units[0].text = "alpha generated caption only in fused tail".into();
    tail.evidence_units[0].text_hash =
        verbatim_core::types::hex_sha256(tail.evidence_units[0].text.as_bytes());
    tail.evidence_units[0].derived_from = Some(EvidenceId("first-support".into()));
    tail.chunk.text = tail.evidence_units[0].text.clone();

    let all_results = vec![first, second, tail];
    let evidence = all_results
        .iter()
        .flat_map(|result| result.evidence_units.clone())
        .collect::<Vec<_>>();
    let chunks = all_results
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
    pipeline.lexical_index().rebuild_from_store(store).unwrap();

    let controls = EffectiveRetrieveControls {
        limit: 3,
        page_size: 1,
        page,
        include_debug: true,
        include_debug_packs,
        include_locator: true,
        passage: false,
        bypass_cache: false,
        fast: false,
        retrieval_config: config.retrieval.clone(),
        rerank_config: config.rerank.clone(),
        config: config.clone(),
    };
    let embed_client = OpenAiEmbeddingClient::new(&config.embedding);
    let lexical_index = pipeline.lexical_index();
    let retrieval = RetrievalPipeline::new(
        pipeline.vector_index(),
        &lexical_index,
        store,
        &embed_client,
        &config.retrieval,
    )
    .with_embedding_enabled(false);
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let (results, debug) = runtime
        .block_on(retrieval.search_source_set_with_debug_options(
            "alpha",
            None,
            retrieve_debug_options(&controls),
        ))
        .unwrap();
    drop(pipeline);
    let store = Store::open_existing_readonly(&test_dir.path().join("verbatim.db")).unwrap();
    (store, results, debug, controls)
}
