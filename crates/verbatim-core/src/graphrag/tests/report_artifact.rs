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
