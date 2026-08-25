fn graph_report_and_seed_results() -> Vec<RetrievalResult> {
    let results = sample_results();
    let mut graph = results[0].clone();
    let mut seed = results[0].clone();
    graph.evidence_units.truncate(1);
    graph.provenance = crate::retrieve::graph_report_provenance(
        1,
        crate::types::report_artifact::ReportArtifactId::new("community-1").unwrap(),
    );
    seed.evidence_units.remove(0);
    seed.chunk_id = ChunkId("c2".into());
    seed.provenance = RetrievalProvenance::seed(2, seed.chunk_id.clone(), SourceId("src".into()));
    vec![graph, seed]
}

#[test]
fn source_pack_labels_preserve_retrieve_role() {
    let pack = build_source_pack(
        &graph_report_and_seed_results(),
        &GenerationContext::default(),
        false,
    );

    assert!(pack.text.contains("[E1 | graph_report |"), "{}", pack.text);
    assert!(pack.text.contains("[E2 | original_text |"), "{}", pack.text);
    assert!(pack.text.contains("original_text:\nFreedom is defined"));
    assert!(!pack.text.contains("generated_text:\n"));
}

#[test]
fn ask_citations_preserve_retrieve_role() {
    let pack = build_source_pack(
        &graph_report_and_seed_results(),
        &GenerationContext::default(),
        false,
    );
    let citations = extract_citations("See [E1] and [E2].", &pack.evidence_refs);
    let encoded = serde_json::to_value(&citations).unwrap();

    assert_eq!(encoded[0]["role"], "graph_report");
    assert_eq!(encoded[0]["evidence_id"], "graphrag://report/community-1");
    assert_eq!(encoded[1]["role"], "original_text");
}
