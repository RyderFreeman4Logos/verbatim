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
    let manifest = service
        .resolve_report_artifact(&artifact_id)
        .unwrap()
        .expect("resolved artifact manifest");
    assert_eq!(manifest.id, artifact_id);
    assert_eq!(manifest.report, report);
    assert_eq!(manifest.generation, report.generation);
    assert_eq!(manifest.content_hash, report.content_hash);
    // The hash verifies against recomputation over the payload.
    assert_eq!(
        manifest.report.recompute_content_hash().unwrap(),
        manifest.content_hash
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
        service
            .resolve_report_artifact(&artifact_id)
            .unwrap()
            .unwrap()
            .report,
        report
    );

    let report = service
        .global_search("alpha", Some(&source_a.id))
        .unwrap()
        .pop()
        .unwrap()
        .report;
    let artifact_id = ReportArtifactId::new(&report.id).unwrap();
    assert_eq!(
        service
            .resolve_report_artifact(&artifact_id)
            .unwrap()
            .unwrap()
            .report,
        report
    );
}

#[test]
fn community_report_records_generation_and_content_hash_manifest() {
    let store = Store::in_memory().unwrap();
    let source = source("src");
    let config = enabled_config();
    insert_chunk(&store, &source, "chunk-a", "Climate evidence.");
    store
        .upsert_graph_nodes(std::slice::from_ref(&generated_claim(
            &source.id,
            "Climate reports discuss rainfall trends.",
            "Climate",
            "Rainfall",
            "chunk-a:1-1",
        )))
        .unwrap();
    let service = GraphRagService::new(&store, &config);

    let reports = service.community_reports(None).unwrap();
    assert_eq!(reports.len(), 1);
    let report = reports[0].clone();
    assert!(!report.content_hash.is_empty());
    assert!(!report.generation.is_empty());
    assert_eq!(
        report.recompute_content_hash().unwrap(),
        report.content_hash
    );

    // Identical graph state ⇒ identical manifest.
    let again = service.community_reports(None).unwrap().pop().unwrap();
    assert_eq!(again.generation, report.generation);
    assert_eq!(again.content_hash, report.content_hash);
}

#[test]
fn report_generation_and_content_hash_change_when_graph_mutates() {
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

    let before = service.community_reports(None).unwrap();
    assert_eq!(before.len(), 1);
    let before = before[0].clone();

    let extra = generated_claim(
        &source.id,
        "Climate reports also mention storm surges.",
        "Climate",
        "Rainfall",
        "chunk-a:1-1",
    );
    store
        .upsert_graph_nodes(std::slice::from_ref(&extra))
        .unwrap();
    let after = service.community_reports(None).unwrap();
    assert_eq!(after.len(), 1);
    let after = after[0].clone();

    assert_ne!(after.generation, before.generation);
    assert_ne!(after.content_hash, before.content_hash);
}

#[test]
fn legacy_community_report_json_deserializes_with_defaulted_manifest_fields() {
    let legacy = r#"{"id":"c1","title":"t","summary":"s","claims":[],"evidence":[]}"#;
    let report: CommunityReport = serde_json::from_str(legacy).unwrap();
    assert_eq!(report.content_hash, "");
    assert_eq!(report.generation, "");
}
