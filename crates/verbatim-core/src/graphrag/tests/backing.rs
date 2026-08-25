use super::*;

#[test]
fn backing_results_use_best_report_order_and_deduplicate_evidence() {
    let store = Store::in_memory().unwrap();
    let source = source("src");
    let alpha = insert_chunk(&store, &source, "chunk-a", "Alpha evidence.");
    let beta = insert_chunk(&store, &source, "chunk-b", "Beta evidence.");
    let alpha_evidence = store
        .get_evidence(&alpha.evidence_unit_ids[0])
        .unwrap()
        .unwrap();
    let beta_evidence = store
        .get_evidence(&beta.evidence_unit_ids[0])
        .unwrap()
        .unwrap();
    let backing = |chunk: &Chunk, evidence: &EvidenceUnit| CommunityReportEvidence {
        source_span: GraphSourceSpan {
            raw: format!("{}:1-1", chunk.id.0),
            chunk_id: chunk.id.clone(),
            line_start: 1,
            line_end: 1,
        },
        evidence: evidence.clone(),
    };
    let report = |id: &str, evidence| CommunityReport {
        id: id.into(),
        title: id.into(),
        summary: id.into(),
        claims: Vec::new(),
        evidence,
        content_hash: String::new(),
        generation: String::new(),
    };
    let hits = vec![
        GlobalSearchHit {
            rank: 2,
            score: 1.0,
            report_artifact_id: ReportArtifactId::new("lower").unwrap(),
            report: report("lower", vec![backing(&alpha, &alpha_evidence)]),
        },
        GlobalSearchHit {
            rank: 1,
            score: 4.0,
            report_artifact_id: ReportArtifactId::new("best").unwrap(),
            report: report(
                "best",
                vec![
                    backing(&beta, &beta_evidence),
                    backing(&alpha, &alpha_evidence),
                ],
            ),
        },
    ];

    let results = backing_results_from_hits(&store, hits, 2).unwrap();

    assert_eq!(results.len(), 2);
    assert_eq!(results[0].evidence_units, vec![beta_evidence]);
    assert_eq!(results[1].evidence_units, vec![alpha_evidence]);
    assert_eq!(results[0].score, 4.0);
    assert_eq!(results[1].score, 4.0);
    assert_eq!(results[0].provenance.result_rank, 1);
    assert_eq!(results[1].provenance.result_rank, 2);
    assert_eq!(
        results[0]
            .provenance
            .report_artifact_id
            .as_ref()
            .unwrap()
            .as_str(),
        "graphrag://report/best"
    );
}

#[test]
fn global_search_backing_results_respect_source_scope() {
    let store = Store::in_memory().unwrap();
    let included = source("included");
    let excluded = source("excluded");
    insert_chunk(&store, &included, "included-chunk", "Included evidence.");
    insert_chunk(&store, &excluded, "excluded-chunk", "Excluded evidence.");
    let claims = [
        generated_claim(
            &included.id,
            "Scoped concept support.",
            "Scoped",
            "Support",
            "included-chunk:1-1",
        ),
        generated_claim(
            &excluded.id,
            "Scoped concept support.",
            "Scoped",
            "Support",
            "excluded-chunk:1-1",
        ),
    ];
    store.upsert_graph_nodes(&claims).unwrap();
    let config = enabled_config();
    let service = GraphRagService::new(&store, &config);
    let source_filter = HashSet::from([included.id.clone()]);

    let results = service
        .global_search_backing_results("scoped concept", Some(&source_filter))
        .unwrap();

    assert!(!results.is_empty());
    assert!(results
        .iter()
        .flat_map(|result| &result.evidence_units)
        .all(|evidence| evidence.source_id == included.id));
}
