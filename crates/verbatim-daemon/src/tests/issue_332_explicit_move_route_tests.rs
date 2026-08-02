use axum::body::{to_bytes, Body};
use axum::extract::connect_info::ConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use tower::ServiceExt;
use verbatim_core::api::{
    ErrorResponse, EvidenceResponse, RetrieveRequest, RetrieveResponse, SourceResponse,
};

use super::*;

async fn issue_332_request(
    app: &Router,
    method: Method,
    uri: &str,
    body: impl Serialize,
) -> Response {
    let mut request = Request::builder()
        .method(method)
        .uri(uri)
        .header(header::CONTENT_TYPE, "application/json")
        .body(Body::from(serde_json::to_vec(&body).unwrap()))
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:43210".parse::<SocketAddr>().unwrap(),
    ));
    app.clone().oneshot(request).await.unwrap()
}

async fn issue_332_body<T: serde::de::DeserializeOwned>(response: Response) -> T {
    let bytes = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();
    serde_json::from_slice(&bytes).unwrap()
}

fn issue_332_retrieve_request(source_id: &SourceId) -> RetrieveRequest {
    RetrieveRequest {
        question: "Where is the relocation boundary evidence?".into(),
        source_id: Some(source_id.0.clone()),
        collection_filter: CollectionFilterRequest::default(),
        embedding_profile_id: None,
        limit: Some(3),
        page_size: Some(3),
        page: Some(1),
        fast: true,
        rerank: Some(false),
        dense_top_k: None,
        bm25_top_k: None,
        rerank_top_n: None,
        bypass_cache: false,
        include_debug: false,
        include_debug_packs: false,
        include_locator: true,
        passage: false,
    }
}

#[tokio::test]
async fn issue_332_existing_source_routes_keep_raw_id_semantics() {
    let test_dir = TestDir::new("issue-332-route-raw-source-id");
    let source_path = test_dir.path().join("legacy.md");
    fs::write(&source_path, "legacy route source").unwrap();
    let source_id = SourceId("legacy-source-id".into());
    let config = retrieve_test_config("http://127.0.0.1:9/v1");
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    pipeline
        .store()
        .add_source(&verbatim_core::types::Source {
            id: source_id.clone(),
            path: fs::canonicalize(&source_path).unwrap(),
            hash: "legacy-hash".into(),
            status: SourceStatus::Pending,
            parser_used: None,
            last_ingested_at: None,
        })
        .unwrap();
    let state = test_state(config, test_dir.path(), pipeline);
    let app = daemon_router(Arc::clone(&state));

    let response = issue_332_request(
        &app,
        Method::GET,
        "/api/sources/legacy-source-id",
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let source: SourceResponse = issue_332_body(response).await;
    assert_eq!(source.id, source_id.0);

    let response = issue_332_request(
        &app,
        Method::DELETE,
        "/api/sources/legacy-source-id",
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::NO_CONTENT);
}

#[tokio::test]
async fn issue_332_relocation_accepts_opaque_source_id_in_json_body() {
    let test_dir = TestDir::new("issue-332-route-opaque-source-id");
    let source_path = test_dir.path().join("opaque.md");
    fs::write(&source_path, "opaque route source").unwrap();
    let source_id = SourceId("._.._%雪/?#~prefixed".into());
    let config = retrieve_test_config("http://127.0.0.1:9/v1");
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    pipeline
        .store()
        .add_source(&verbatim_core::types::Source {
            id: source_id.clone(),
            path: fs::canonicalize(&source_path).unwrap(),
            hash: "opaque-hash".into(),
            status: SourceStatus::Pending,
            parser_used: None,
            last_ingested_at: None,
        })
        .unwrap();
    let state = test_state(config, test_dir.path(), pipeline);
    let app = daemon_router(state);

    let response = issue_332_request(
        &app,
        Method::POST,
        "/api/source-relocations",
        serde_json::json!({
            "source_id": source_id.0,
            "new_path": test_dir.path().join("unused.md"),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: ErrorResponse = issue_332_body(response).await;
    assert!(error.error.contains("not indexed"));
}

#[tokio::test]
async fn issue_332_relocation_json_rejections_use_bad_request_error_response() {
    let test_dir = TestDir::new("issue-332-route-json-rejection");
    let config = retrieve_test_config("http://127.0.0.1:9/v1");
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let app = daemon_router(test_state(config, test_dir.path(), pipeline));

    for body in [
        serde_json::json!({}),
        serde_json::json!({ "source_id": 7, "new_path": false }),
    ] {
        let response = issue_332_request(&app, Method::POST, "/api/source-relocations", body).await;

        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        let error: ErrorResponse = issue_332_body(response).await;
        assert!(!error.error.is_empty());
    }
}

#[tokio::test]
async fn issue_332_public_relocation_preserves_retrieval_and_citation_resolution() {
    let model_server = MockModelServer::start(3).await;
    let test_dir = TestDir::new("issue-332-route-success");
    let old_path = test_dir.path().join("before.md");
    let new_path = test_dir.path().join("after.md");
    fs::write(
        &old_path,
        "The relocation boundary evidence remains directly retrievable.",
    )
    .unwrap();
    let config = retrieve_test_config(&model_server.base_url);
    let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let source_id = pipeline.add_source(&old_path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();
    let evidence_id = pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap()[0]
        .id
        .clone();
    let state = test_state(config, test_dir.path(), pipeline);
    fs::rename(&old_path, &new_path).unwrap();
    let app = daemon_router(Arc::clone(&state));

    let response = issue_332_request(
        &app,
        Method::POST,
        "/api/source-relocations",
        serde_json::json!({ "source_id": source_id.0, "new_path": new_path }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::OK);
    let relocated: SourceResponse = issue_332_body(response).await;
    assert_eq!(relocated.id, source_id.0);
    assert_eq!(
        relocated.path,
        fs::canonicalize(&new_path).unwrap().display().to_string()
    );

    let response = issue_332_request(
        &app,
        Method::POST,
        "/api/retrieve",
        issue_332_retrieve_request(&source_id),
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let retrieved: RetrieveResponse = issue_332_body(response).await;
    assert_eq!(retrieved.source_id.as_deref(), Some(source_id.0.as_str()));
    assert_eq!(retrieved.results[0].source_id, source_id.0);
    assert_eq!(retrieved.results[0].evidence_id, evidence_id.0);

    let response = issue_332_request(
        &app,
        Method::GET,
        &format!("/api/evidence/{}", evidence_id.0),
        serde_json::Value::Null,
    )
    .await;
    assert_eq!(response.status(), StatusCode::OK);
    let evidence: EvidenceResponse = issue_332_body(response).await;
    assert_eq!(evidence.id, evidence_id.0);
    assert_eq!(evidence.source_id, source_id.0);
}

#[tokio::test]
async fn issue_332_public_relocation_changed_bytes_preserves_catalog_snapshot() {
    let model_server = MockModelServer::start(3).await;
    let test_dir = TestDir::new("issue-332-route-changed");
    let old_path = test_dir.path().join("before.md");
    let new_path = test_dir.path().join("after.md");
    fs::write(&old_path, "The original indexed relocation bytes.").unwrap();
    let config = retrieve_test_config(&model_server.base_url);
    let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let source_id = pipeline.add_source(&old_path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();
    let source_before = pipeline.store().get_source(&source_id).unwrap().unwrap();
    let evidence_before = pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap();
    let state = test_state(config, test_dir.path(), pipeline);
    fs::rename(&old_path, &new_path).unwrap();
    fs::write(&new_path, "Changed bytes must fail closed.").unwrap();
    let app = daemon_router(Arc::clone(&state));

    let response = issue_332_request(
        &app,
        Method::POST,
        "/api/source-relocations",
        serde_json::json!({ "source_id": source_id.0, "new_path": new_path }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::BAD_REQUEST);
    let error: serde_json::Value = issue_332_body(response).await;
    assert!(error["error"]
        .as_str()
        .unwrap()
        .contains("content hash differs"));
    assert!(error.get("id").is_none());
    assert!(error.get("path").is_none());
    let pipeline = state.pipeline.lock().unwrap();
    let pipeline = pipeline.as_ref().unwrap();
    let source_after = pipeline.store().get_source(&source_id).unwrap().unwrap();
    let evidence_after = pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap();
    assert_eq!(
        serde_json::to_value(source_after).unwrap(),
        serde_json::to_value(source_before).unwrap()
    );
    assert_eq!(
        serde_json::to_value(evidence_after).unwrap(),
        serde_json::to_value(evidence_before).unwrap()
    );
}

#[tokio::test]
async fn issue_332_long_missing_source_id_returns_not_found() {
    let test_dir = TestDir::new("issue-332-route-long-not-found");
    let config = retrieve_test_config("http://127.0.0.1:9/v1");
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let state = test_state(config, test_dir.path(), pipeline);
    let app = daemon_router(state);
    let source_id = "x".repeat(300);

    let response = issue_332_request(
        &app,
        Method::POST,
        "/api/source-relocations",
        serde_json::json!({
            "source_id": source_id,
            "new_path": test_dir.path().join("missing.md"),
        }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::NOT_FOUND);
    let error: serde_json::Value = issue_332_body(response).await;
    assert!(error["error"]
        .as_str()
        .unwrap()
        .contains("source not found"));
}

#[tokio::test]
async fn issue_332_public_relocation_sqlite_failpoint_returns_internal_server_error() {
    let model_server = MockModelServer::start(3).await;
    let test_dir = TestDir::new("issue-332-route-sqlite-failpoint");
    let old_path = test_dir.path().join("before.md");
    let new_path = test_dir.path().join("after.md");
    fs::write(&old_path, "The relocation transaction must roll back.").unwrap();
    let config = retrieve_test_config(&model_server.base_url);
    let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let source_id = pipeline.add_source(&old_path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();
    let source_before = pipeline.store().get_source(&source_id).unwrap().unwrap();
    let evidence_before = pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap();
    rusqlite::Connection::open(test_dir.path().join("verbatim.db"))
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER issue_332_route_relocation_failpoint
             BEFORE UPDATE OF locator_json ON chunk_evidence_spans
             BEGIN
                 SELECT RAISE(ABORT, 'issue-332 route relocation failpoint');
             END;",
        )
        .unwrap();
    let state = test_state(config, test_dir.path(), pipeline);
    fs::rename(&old_path, &new_path).unwrap();
    let app = daemon_router(Arc::clone(&state));

    let response = issue_332_request(
        &app,
        Method::POST,
        "/api/source-relocations",
        serde_json::json!({ "source_id": source_id.0, "new_path": new_path }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
    let error: serde_json::Value = issue_332_body(response).await;
    assert!(error["error"]
        .as_str()
        .unwrap()
        .contains("issue-332 route relocation failpoint"));
    let pipeline = state.pipeline.lock().unwrap();
    let pipeline = pipeline.as_ref().unwrap();
    let source_after = pipeline.store().get_source(&source_id).unwrap().unwrap();
    let evidence_after = pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap();
    assert_eq!(
        serde_json::to_value(source_after).unwrap(),
        serde_json::to_value(source_before).unwrap()
    );
    assert_eq!(
        serde_json::to_value(evidence_after).unwrap(),
        serde_json::to_value(evidence_before).unwrap()
    );
}

#[tokio::test]
async fn issue_332_public_relocation_begin_immediate_contention_returns_unavailable() {
    let model_server = MockModelServer::start(3).await;
    let test_dir = TestDir::new("issue-332-route-sqlite-contention");
    let old_path = test_dir.path().join("before.md");
    let new_path = test_dir.path().join("after.md");
    fs::write(&old_path, "The relocation writer must report contention.").unwrap();
    let mut config = retrieve_test_config(&model_server.base_url);
    config.store.durability = verbatim_core::store::SqliteDurabilityProfile::Ephemeral;
    let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let source_id = pipeline.add_source(&old_path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();
    let source_before = pipeline.store().get_source(&source_id).unwrap().unwrap();
    let evidence_before = pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap();
    let state = test_state(config, test_dir.path(), pipeline);
    fs::rename(&old_path, &new_path).unwrap();
    let app = daemon_router(Arc::clone(&state));
    let blocker = rusqlite::Connection::open(test_dir.path().join("verbatim.db")).unwrap();
    blocker
        .execute_batch("PRAGMA busy_timeout = 0; BEGIN IMMEDIATE;")
        .unwrap();

    let response = issue_332_request(
        &app,
        Method::POST,
        "/api/source-relocations",
        serde_json::json!({ "source_id": source_id.0, "new_path": new_path }),
    )
    .await;

    assert_eq!(response.status(), StatusCode::SERVICE_UNAVAILABLE);
    let error: ErrorResponse = issue_332_body(response).await;
    assert!(error.error.contains("database is locked"));
    blocker.execute_batch("ROLLBACK;").unwrap();
    let pipeline = state.pipeline.lock().unwrap();
    let pipeline = pipeline.as_ref().unwrap();
    let source_after = pipeline.store().get_source(&source_id).unwrap().unwrap();
    let evidence_after = pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap();
    assert_eq!(
        serde_json::to_value(source_after).unwrap(),
        serde_json::to_value(source_before).unwrap()
    );
    assert_eq!(
        serde_json::to_value(evidence_after).unwrap(),
        serde_json::to_value(evidence_before).unwrap()
    );
}
