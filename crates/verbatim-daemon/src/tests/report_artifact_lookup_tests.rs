use super::*;
use verbatim_core::config::GraphGlobalSearchConfig;
use verbatim_core::graphrag::{CommunityReport, GraphRagService};
use verbatim_core::types::report_artifact::ReportArtifactId;

const CANONICAL_MISSING: &str = "graphrag://report/community-test";
const LEGACY_MISSING: &str = "graphrag:report:community-test";
const SECRET_MISSING: &str = "graphrag://report/secret-token";

#[tokio::test]
async fn canonical_reserved_report_artifact_id_reaches_artifact_handler() {
    let app = missing_artifact_app("report-artifact-canonical");
    assert_typed_artifact_miss(&app, CANONICAL_MISSING).await;
}

#[tokio::test]
async fn legacy_reserved_report_artifact_id_reaches_artifact_handler() {
    let app = missing_artifact_app("report-artifact-legacy");
    assert_typed_artifact_miss(&app, LEGACY_MISSING).await;
}

#[tokio::test]
async fn missing_report_artifact_returns_typed_404_not_evidence_unit() {
    let app = missing_artifact_app("report-artifact-missing");
    let (status, body, error) = artifact_route_error(&app, CANONICAL_MISSING).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(
        error.code.as_deref(),
        Some("report_artifact_not_found"),
        "missing artifact must use a closed code: {error:?}"
    );
    assert!(
        error.error.contains("report artifact"),
        "typed 404 must name report artifacts: {error:?}"
    );
    assert_ne_evidence_unit(&body);
    let debug = format!("{error:?}");
    assert!(
        debug.contains(CANONICAL_MISSING),
        "caller-supplied id may appear in diagnostics: {debug}"
    );
}

#[tokio::test]
async fn present_report_artifact_returns_manifest() {
    let (test_dir, store, persisted) = persisted_output_fixture("report-artifact-present", None);
    let chunk_id = persisted.chunk.id.0.clone();
    let source_id = persisted.evidence_units[0].source_id.clone();
    let claim = generated_claim_node(&source_id, &chunk_id);
    store
        .upsert_graph_nodes(std::slice::from_ref(&claim))
        .unwrap();
    let graph_config = GraphGlobalSearchConfig {
        enabled: true,
        ..GraphGlobalSearchConfig::default()
    };
    let report = GraphRagService::new(&store, &graph_config)
        .community_reports(None)
        .unwrap()
        .pop()
        .expect("seeded claim must produce a community report");
    let artifact_id = ReportArtifactId::new(&report.id).unwrap();
    drop(store);

    let app = artifact_test_app(test_dir.path(), true);
    let response = artifact_route_get(&app, artifact_id.as_str()).await;
    let status = response.status();
    let body = evidence_route_body(response).await;
    let body_text = String::from_utf8_lossy(&body);
    assert_eq!(
        status,
        StatusCode::OK,
        "present report artifact must be 200, got {status}: {body_text}"
    );
    let wire: serde_json::Value = serde_json::from_slice(&body).unwrap_or_else(|_| {
        panic!(
            "present artifact must return ReportArtifactManifest JSON, got {status}: {body_text}"
        )
    });
    assert_eq!(wire["id"], artifact_id.as_str());
    assert_eq!(
        wire["schema_version"],
        serde_json::json!({
            "major": 1,
            "minor": 0,
            "patch": 0
        })
    );
    assert_eq!(wire["derived_kind"], "graph_report");
    assert_eq!(wire["report"]["id"], report.id);
    let report_body: CommunityReport = serde_json::from_value(wire["report"].clone()).unwrap();
    assert_eq!(wire["identity"]["kind"], "derived_artifact");
    assert_eq!(wire["identity"]["schema_version"], wire["schema_version"]);
    assert_eq!(wire["identity"]["artifact_id"], wire["id"]);
    assert_eq!(
        wire["identity"]["content_hash"],
        report_body.recompute_content_hash().unwrap()
    );
    assert_ne_evidence_unit(&body);
}

#[tokio::test]
async fn stored_report_payload_identity_mismatch_fails_closed() {
    let (test_dir, store, persisted) = persisted_output_fixture("report-artifact-corrupt", None);
    let chunk_id = persisted.chunk.id.0.clone();
    let source_id = persisted.evidence_units[0].source_id.clone();
    let claim = generated_claim_node(&source_id, &chunk_id);
    store
        .upsert_graph_nodes(std::slice::from_ref(&claim))
        .unwrap();
    let graph_config = GraphGlobalSearchConfig {
        enabled: true,
        ..GraphGlobalSearchConfig::default()
    };
    let report = GraphRagService::new(&store, &graph_config)
        .community_reports(None)
        .unwrap()
        .pop()
        .expect("seeded claim must produce a community report");
    let artifact_id = ReportArtifactId::new(&report.id).unwrap();
    let app = artifact_test_app(test_dir.path(), true);

    assert_eq!(
        artifact_route_get(&app, artifact_id.as_str())
            .await
            .status(),
        StatusCode::OK
    );

    let mut corrupted_report = report;
    corrupted_report.summary = "corrupt stored report payload".into();
    let changed = rusqlite::Connection::open(test_dir.path().join("verbatim.db"))
        .unwrap()
        .execute(
            "UPDATE report_artifacts SET payload_json = ?1
             WHERE report_id = ?2 AND generation = ?3 AND content_hash = ?4",
            [
                serde_json::to_string(&corrupted_report).unwrap(),
                artifact_id.as_str().to_string(),
                corrupted_report.generation.clone(),
                corrupted_report.content_hash.clone(),
            ],
        )
        .unwrap();
    assert_eq!(changed, 1, "initial GET must persist the report payload");

    let response = artifact_route_get(&app, artifact_id.as_str()).await;
    let status = response.status();
    let body = evidence_route_body(response).await;
    let body_text = String::from_utf8_lossy(&body);
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body_text}");
    assert!(
        !body_text.contains(&corrupted_report.summary),
        "mismatched stored report body must not be returned: {body_text}"
    );
}

#[tokio::test]
async fn reserved_report_artifact_ids_on_evidence_route_stay_typed_4xx() {
    let app = missing_artifact_app("report-artifact-evidence-still-4xx");
    for id in [CANONICAL_MISSING, LEGACY_MISSING] {
        let response = evidence_route_get(&app, id).await;
        let status = response.status();
        let body = evidence_route_body(response).await;
        let body_text = String::from_utf8_lossy(&body);
        let error: ErrorResponse = serde_json::from_slice(&body).unwrap_or_else(|_| {
            panic!("evidence path must stay JSON 4xx for reserved ids, got {status}: {body_text}")
        });
        assert!(
            status.is_client_error(),
            "reserved id on evidence must be 4xx, got {status}: {}",
            error.error
        );
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "reserved id on evidence is not missing evidence: {}",
            error.error
        );
        assert!(
            error.error.contains("report artifact"),
            "evidence 4xx must say these IDs are report artifacts: {error:?}"
        );
        assert!(
            error.error.contains("not evidence"),
            "evidence 4xx must say these IDs are not evidence: {error:?}"
        );
        let wire: serde_json::Value = serde_json::from_slice(&body).unwrap();
        assert!(
            wire.get("derived_kind").is_none(),
            "evidence route must never return ReportArtifactManifest: {body_text}"
        );
    }
}

#[tokio::test]
async fn secret_bearing_report_artifact_id_diagnostics_stay_closed() {
    let app = missing_artifact_app("report-artifact-secret");
    let (status, _body, error) = artifact_route_error(&app, SECRET_MISSING).await;
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error.code.as_deref(), Some("report_artifact_not_found"));
    let rendered = format!("{error:?}");
    assert!(
        rendered.contains("secret-token"),
        "caller-supplied id may appear: {rendered}"
    );
    assert!(
        !rendered.contains("payload_json") && !rendered.contains("verbatim.db"),
        "diagnostics must not leak store internals: {rendered}"
    );
}

#[tokio::test]
async fn busy_pipeline_returns_service_unavailable() {
    let test_dir = TestDir::new("report-artifact-pipeline-busy");
    let config = retrieve_test_config("http://127.0.0.1:9/v1");
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let state = test_state(config, test_dir.path(), pipeline);
    let pipeline = take_pipeline(&state).expect("take pipeline slot");
    let app = daemon_router(Arc::clone(&state));

    let response = artifact_route_get(&app, CANONICAL_MISSING).await;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    restore_pipeline(&state, pipeline).expect("restore pipeline slot");
}

fn missing_artifact_app(name: &str) -> Router {
    let (test_dir, store, _persisted) = persisted_output_fixture(name, None);
    drop(store);
    artifact_test_app(test_dir.path(), false)
}

fn artifact_test_app(data_dir: &std::path::Path, enable_graph: bool) -> Router {
    let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
    config.graph.global_search.enabled = enable_graph;
    let pipeline = IngestPipeline::new(&config, data_dir).unwrap();
    daemon_router(test_state(config, data_dir, pipeline))
}

fn generated_claim_node(source_id: &SourceId, chunk_id: &str) -> GraphNode {
    let external_id = "generated_claim:report-artifact-lookup";
    GraphNode {
        id: GraphNodeId::new(source_id, GraphNodeKind::GeneratedClaim, external_id),
        source_id: source_id.clone(),
        kind: GraphNodeKind::GeneratedClaim,
        external_id: external_id.into(),
        label: Some("Climate reports discuss rainfall trends.".into()),
        locator: None,
        ordinal: None,
        metadata: Some(serde_json::json!({
            "origin": "llm_generated",
            "graph_data_kind": "claim",
            "claim": "Climate reports discuss rainfall trends.",
            "subject": "Climate",
            "predicate": "supports",
            "object": "Rainfall",
            "source_spans": [format!("{chunk_id}:1-1")]
        })),
    }
}

async fn assert_typed_artifact_miss(app: &Router, id: &str) {
    let (status, body, error) = artifact_route_error(app, id).await;
    assert!(
        status.is_client_error(),
        "reserved artifact id must reach the artifact handler as JSON 4xx, got {status}: {}",
        error.error
    );
    assert_eq!(status, StatusCode::NOT_FOUND);
    assert_eq!(error.code.as_deref(), Some("report_artifact_not_found"));
    assert_ne_evidence_unit(&body);
}

async fn artifact_route_error(app: &Router, id: &str) -> (StatusCode, Vec<u8>, ErrorResponse) {
    let response = artifact_route_get(app, id).await;
    let status = response.status();
    let body = evidence_route_body(response).await;
    let body_text = String::from_utf8_lossy(&body);
    let error = serde_json::from_slice(&body).unwrap_or_else(|_| {
        panic!(
            "reserved report-artifact id must reach the artifact handler as JSON, got {status}: {body_text}"
        )
    });
    (status, body, error)
}

async fn artifact_route_get(app: &Router, artifact_id: &str) -> axum::response::Response {
    let mut request = Request::builder()
        .method(Method::GET)
        .uri(format!("/api/report-artifact/{artifact_id}"))
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:43210".parse::<std::net::SocketAddr>().unwrap(),
    ));
    app.clone().oneshot(request).await.unwrap()
}

fn assert_ne_evidence_unit(body: &[u8]) {
    let wire: serde_json::Value = serde_json::from_slice(body).unwrap();
    assert!(
        wire.get("source_bounded").is_none(),
        "artifact lookup must not return EvidenceUnit: {wire}"
    );
    assert!(
        wire.get("text_taxonomy").is_none(),
        "artifact lookup must not return EvidenceUnit: {wire}"
    );
}
