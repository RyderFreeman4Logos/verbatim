use super::*;
use crate::types::report_artifact::ReportArtifactId;

#[test]
fn report_claims_are_limited_to_resolvable_report_backing_evidence() {
    let store = Store::in_memory().unwrap();
    let source = source("src");
    insert_chunk(&store, &source, "chunk-a", "Alpha evidence.");
    insert_chunk(&store, &source, "chunk-b", "Beta evidence.");
    let claims = [
        generated_claim(
            &source.id,
            "Alpha is supported.",
            "Subject",
            "Object",
            "chunk-a:1-1",
        ),
        generated_claim(
            &source.id,
            "Beta is supported.",
            "Subject",
            "Object",
            "chunk-b:1-1",
        ),
    ];
    let communities = detect_communities(&claims, &[]);
    let mut config = enabled_config();
    config.max_evidence_per_report = 1;

    let reports = build_community_reports(&store, &claims, &[], &communities, &config).unwrap();

    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].claims.len(), 1);
    let backing_ids = reports[0]
        .evidence
        .iter()
        .map(|backing| &backing.evidence.id)
        .collect::<std::collections::BTreeSet<_>>();
    assert!(reports[0].claims.iter().all(|claim| {
        !claim.evidence_ids.is_empty()
            && claim
                .evidence_ids
                .iter()
                .all(|evidence_id| backing_ids.contains(evidence_id))
    }));
}

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
    assert_eq!(
        manifest.derived_kind,
        crate::wire_schemas::DerivedArtifactKind::GraphReport
    );
    // The hash verifies against recomputation over the payload.
    assert_eq!(
        manifest.report.recompute_content_hash().unwrap(),
        manifest.content_hash
    );

    let missing = ReportArtifactId::new("missing-community").unwrap();
    assert_eq!(service.resolve_report_artifact(&missing).unwrap(), None);
}

#[test]
fn report_artifact_json_deserialization_uses_parse_boundary() {
    let legacy: ReportArtifactId =
        serde_json::from_str(r#""graphrag:report:community-test""#).unwrap();
    assert_eq!(legacy.as_str(), "graphrag://report/community-test");
    assert!(serde_json::from_str::<ReportArtifactId>(r#""not-an-artifact-id""#).is_err());

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
    let report = service.community_reports(None).unwrap().pop().unwrap();
    let artifact_id = ReportArtifactId::new(&report.id).unwrap();
    let manifest = service
        .resolve_report_artifact(&artifact_id)
        .unwrap()
        .unwrap();
    let mut wire = serde_json::to_value(manifest).unwrap();

    wire["id"] = serde_json::json!("graphrag:report:community-test");
    let normalized: ReportArtifactManifest = serde_json::from_value(wire.clone()).unwrap();
    assert_eq!(normalized.id.as_str(), "graphrag://report/community-test");

    wire["id"] = serde_json::json!("not-an-artifact-id");
    assert!(serde_json::from_value::<ReportArtifactManifest>(wire).is_err());
}

#[test]
fn resolve_report_artifact_persists_manifest_by_generation_and_content_hash() {
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
    let report = service.community_reports(None).unwrap().pop().unwrap();
    let artifact_id = ReportArtifactId::new(&report.id).unwrap();

    let manifest = service
        .resolve_report_artifact(&artifact_id)
        .unwrap()
        .expect("resolved artifact manifest");
    let (generation, content_hash, payload): (String, String, String) = store
        .connection()
        .query_row(
            "SELECT generation, content_hash, payload_json
             FROM report_artifacts
             WHERE report_id = ?1",
            [artifact_id.as_str()],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(generation, manifest.generation);
    assert_eq!(content_hash, manifest.content_hash);
    assert_eq!(
        serde_json::from_str::<CommunityReport>(&payload).unwrap(),
        manifest.report
    );

    assert_eq!(
        service.resolve_report_artifact(&artifact_id).unwrap(),
        Some(manifest)
    );
    let rows: u64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM report_artifacts WHERE report_id = ?1",
            [artifact_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 1);
}

#[test]
fn remove_source_deletes_its_persisted_report_artifact() {
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
    let report = service.community_reports(None).unwrap().pop().unwrap();
    let artifact_id = ReportArtifactId::new(&report.id).unwrap();

    service.resolve_report_artifact(&artifact_id).unwrap();
    store.remove_source(&source.id).unwrap();

    let rows: u64 = store
        .connection()
        .query_row(
            "SELECT COUNT(*) FROM report_artifacts WHERE report_id = ?1",
            [artifact_id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
    assert_eq!(service.resolve_report_artifact(&artifact_id).unwrap(), None);
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
    let mut payload = serde_json::to_value(&report).unwrap();
    payload.as_object_mut().unwrap().remove("content_hash");
    assert_eq!(
        hex_sha256(&serde_json::to_vec(&payload).unwrap()),
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
