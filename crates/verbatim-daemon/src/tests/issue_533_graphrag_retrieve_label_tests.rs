use super::*;

#[tokio::test]
async fn graphrag_retrieve_results_publish_graph_report_without_relabeling_seed_neighbors() {
    let model_server = MockModelServer::start(3).await;
    let test_dir = TestDir::new("issue-533-graphrag-retrieve-label");
    let source_path = test_dir.path().join("cedar.md");
    fs::write(
        &source_path,
        "The cedar protocol is backed by this authoritative stored passage.",
    )
    .unwrap();
    let distractor_path = test_dir.path().join("distractor.md");
    fs::write(
        &distractor_path,
        "Zirconium provenance appears here as a lexical distractor only.",
    )
    .unwrap();
    let mut config = retrieve_test_config(&model_server.base_url);
    config.embedding.enabled = false;
    config.rerank.enabled = false;
    config.graph.global_search.enabled = true;

    let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let source_id = pipeline.add_source(&source_path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();
    let distractor_source_id = pipeline.add_source(&distractor_path).unwrap();
    pipeline.ingest_source(&distractor_source_id).await.unwrap();
    let chunk = pipeline
        .store()
        .list_chunks_by_source(&source_id)
        .unwrap()
        .into_iter()
        .find(|chunk| chunk.chunk_type == ChunkType::Child)
        .expect("ingest creates a child chunk");
    let graph_report_prose =
        "Generated graph claim selects the cedar passage for zirconium provenance.";
    let external_id = "generated_claim:cedar-provenance";
    let claim = GraphNode {
        id: GraphNodeId::new(&source_id, GraphNodeKind::GeneratedClaim, external_id),
        source_id: source_id.clone(),
        kind: GraphNodeKind::GeneratedClaim,
        external_id: external_id.into(),
        label: Some(graph_report_prose.into()),
        locator: None,
        ordinal: None,
        metadata: Some(serde_json::json!({
            "origin": "llm_generated",
            "graph_data_kind": "claim",
            "claim": graph_report_prose,
            "subject": "cedar passage",
            "predicate": "supports",
            "object": "zirconium provenance",
            "source_spans": [format!("{}:1-1", chunk.id.0)]
        })),
    };
    pipeline
        .store()
        .upsert_graph_nodes(std::slice::from_ref(&claim))
        .unwrap();
    let report_artifact_id = GraphRagService::new(pipeline.store(), &config.graph.global_search)
        .global_search("What supports zirconium provenance?", None)
        .unwrap()[0]
        .report_artifact_id
        .as_str()
        .to_owned();
    let graph_evidence_id = chunk.evidence_unit_ids[0].clone();

    let state = test_state(config, test_dir.path(), pipeline);
    let Json(response) = retrieve(
        State(Arc::clone(&state)),
        Json(context_only_retrieve_request(AskRequest {
            question: "What supports zirconium provenance?".into(),
            source_id: None,
            collection_filter: CollectionFilterRequest::default(),
            embedding_profile_id: None,
            show_retrieval: true,
            context_only: true,
            limit: Some(3),
            page_size: Some(3),
            page: None,
        })),
    )
    .await
    .unwrap();

    let graph_result = response
        .results
        .iter()
        .find(|result| result.evidence_id == graph_evidence_id.0)
        .expect("GraphRAG-backed result");
    assert_eq!(graph_result.role, "graph_report");
    assert_eq!(
        serde_json::to_value(&graph_result.provenance).unwrap()["origin"],
        "graph_report"
    );
    let seed_result = response
        .results
        .iter()
        .find(|result| result.source_id == distractor_source_id.0)
        .expect("ordinary BM25 result");
    assert_eq!(
        serde_json::to_value(&seed_result.provenance).unwrap()["origin"],
        "seed"
    );

    let debug = response.debug.expect("retrieve debug output");
    let debug = serde_json::to_value(debug).unwrap();
    let debug_pack = debug["final_evidence_pack"]
        .as_array()
        .filter(|pack| !pack.is_empty())
        .unwrap_or_else(|| debug["display_evidence_pack"].as_array().unwrap());
    let graph_entry = debug_pack
        .iter()
        .find(|entry| entry["provenance"]["report_artifact_id"] == report_artifact_id)
        .expect("GraphRAG-backed debug entry");
    assert_eq!(graph_entry["role"], "graph_report");
    assert_eq!(graph_entry["provenance"]["origin"], "graph_report");
    let seed_entry = debug_pack
        .iter()
        .find(|entry| entry["source_id"] == distractor_source_id.0)
        .expect("ordinary BM25 debug entry");
    assert_eq!(seed_entry["provenance"]["origin"], "seed");
}
