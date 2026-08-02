use axum::body::{to_bytes, Body};
use axum::extract::connect_info::ConnectInfo;
use axum::http::{header, Method, Request, StatusCode};
use tower::ServiceExt;
use verbatim_core::api::{EvidenceResponse, RetrieveRequest, RetrieveResponse, SourceResponse};

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
        &format!("/api/sources/{}/relocate", source_id.0),
        serde_json::json!({ "new_path": new_path }),
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
        &format!("/api/sources/{}/relocate", source_id.0),
        serde_json::json!({ "new_path": new_path }),
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
