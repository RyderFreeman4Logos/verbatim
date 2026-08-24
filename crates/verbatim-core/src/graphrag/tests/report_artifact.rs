use super::*;
use crate::types::report_artifact::ReportArtifactId;

#[test]
fn resolve_report_artifact_reconstructs_existing_report_and_returns_none_when_missing() {
    let store = Store::in_memory().unwrap();
    let source = source("src");
    let config = enabled_config();
    insert_chunk(&store, &source, "chunk-a", "Climate evidence.");
    let claim = generated_claim(
        &source.id,
        "Climate reports discuss rainfall trends.",
        "Climate",
        "Rainfall",
        "chunk-a:1-1",
    );
    store
        .upsert_graph_nodes(std::slice::from_ref(&claim))
        .unwrap();
    let service = GraphRagService::new(&store, &config);
    let report = service.community_reports(None).unwrap().pop().unwrap();

    let artifact_id = ReportArtifactId::new(&report.id).unwrap();
    assert_eq!(
        service.resolve_report_artifact(&artifact_id).unwrap(),
        Some(report)
    );

    let missing = ReportArtifactId::new("missing-community").unwrap();
    assert_eq!(service.resolve_report_artifact(&missing).unwrap(), None);
}

#[test]
fn resolve_source_scoped_report_artifact_when_matching_entities_exist_in_another_source() {
    let store = Store::in_memory().unwrap();
    let source_a = source("source-a");
    let source_b = source("source-b");
    let config = enabled_config();
    insert_chunk(
        &store,
        &source_a,
        "chunk-a",
        "Alpha evidence from source A.",
    );
    insert_chunk(
        &store,
        &source_b,
        "chunk-b",
        "Alpha evidence from source B.",
    );
    let nodes = [
        generated_entity(&source_a.id, "Alpha", "concept", "chunk-a:1-1"),
        generated_claim(
            &source_a.id,
            "Alpha has evidence from source A.",
            "Alpha",
            "Source A",
            "chunk-a:1-1",
        ),
        generated_entity(&source_b.id, "Alpha", "concept", "chunk-b:1-1"),
        generated_claim(
            &source_b.id,
            "Alpha has evidence from source B.",
            "Alpha",
            "Source B",
            "chunk-b:1-1",
        ),
    ];
    store.upsert_graph_nodes(&nodes).unwrap();
    let service = GraphRagService::new(&store, &config);

    let report = service
        .community_reports(Some(&source_a.id))
        .unwrap()
        .pop()
        .unwrap();
    let artifact_id = ReportArtifactId::new(&report.id).unwrap();
    assert_eq!(
        service.resolve_report_artifact(&artifact_id).unwrap(),
        Some(report.clone())
    );

    let report = service
        .global_search("alpha", Some(&source_a.id))
        .unwrap()
        .pop()
        .unwrap()
        .report;
    let artifact_id = ReportArtifactId::new(&report.id).unwrap();
    assert_eq!(
        service.resolve_report_artifact(&artifact_id).unwrap(),
        Some(report)
    );
}
