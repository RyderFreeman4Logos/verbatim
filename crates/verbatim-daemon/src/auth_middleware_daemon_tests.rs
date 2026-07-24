use super::*;
use axum::body::Body;
use axum::extract::connect_info::ConnectInfo;
use axum::http::Request as HttpRequest;
use tower::ServiceExt;

#[tokio::test]
async fn daemon_router_enforces_loopback_guard_and_health_exemption() {
    let test_dir = TestDir::new("daemon-router-auth");
    let config = retrieve_test_config("http://127.0.0.1:9/v1");
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let app = daemon_router(test_state(config, test_dir.path(), pipeline));

    let mut remote_config_request = HttpRequest::builder()
        .uri("/api/config")
        .body(Body::empty())
        .unwrap();
    remote_config_request.extensions_mut().insert(ConnectInfo(
        "192.0.2.1:43210".parse::<SocketAddr>().unwrap(),
    ));
    let remote_response = app.clone().oneshot(remote_config_request).await.unwrap();
    assert_eq!(remote_response.status(), StatusCode::FORBIDDEN);

    let health_response = app
        .oneshot(
            HttpRequest::builder()
                .uri("/api/health")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(health_response.status(), StatusCode::OK);
}
