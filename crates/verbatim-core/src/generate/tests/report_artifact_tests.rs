#[test]
fn verifier_source_inputs_reject_report_artifact_ids() {
    let mut citations = extract_citations(
        "Known [E1].",
        &build_source_pack(&sample_results(), &GenerationContext::default(), false).evidence_refs,
    );
    citations[0].evidence_id = EvidenceId("graphrag://report/community-1".into());
    assert!(verifier_source_inputs(&citations, &[]).is_empty());
}

#[test]
fn graph_report_context_pack_uses_canonical_artifact_id() {
    let mut results = sample_results();
    results[0].provenance.origin = crate::types::RetrievalOrigin::GraphReport;
    results[0].provenance.report_artifact_id = Some(
        crate::types::report_artifact::ReportArtifactId::parse("graphrag:report:legacy-community")
            .unwrap(),
    );

    let pack = build_source_pack(&results[..1], &GenerationContext::default(), false);
    let citations = extract_citations("Known [E1].", &pack.evidence_refs);

    assert_eq!(
        citations[0].evidence_id.0,
        "graphrag://report/legacy-community"
    );
    assert_eq!(citations[0].role, RetrievalEvidenceRole::GraphReport);
}

#[test]
fn source_pack_includes_all_evidence() {
    let pack = build_source_pack(&sample_results(), &GenerationContext::default(), false);
    assert!(pack.text.contains("[E1 | original_text |"));
    assert!(pack.text.contains("[E2 | original_text |"));
    assert!(pack.text.contains("original_text:\nFreedom is defined"));
    assert_eq!(pack.evidence_refs.len(), 2);
}

#[test]
fn extract_cited_references() {
    let pack = build_source_pack(&sample_results(), &GenerationContext::default(), false);
    let answer = "The concept [E1, E2] is important.";
    let citations = extract_citations(answer, &pack.evidence_refs);
    assert_eq!(citations.len(), 2);
    assert_eq!(citations[0].label, "E1");
    assert_eq!(citations[1].label, "E2");
}
