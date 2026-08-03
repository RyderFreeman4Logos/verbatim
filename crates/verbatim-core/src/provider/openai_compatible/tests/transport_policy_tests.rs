use super::*;

fn spawn_redirect_server(location: String) -> (String, thread::JoinHandle<RecordedRequest>) {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind redirect server");
    let base_url = format!("http://{}", listener.local_addr().expect("server addr"));
    let handle = thread::spawn(move || {
        let (mut stream, _) = listener.accept().expect("accept request");
        let request = read_http_request(&mut stream);
        let response = format!(
            "HTTP/1.1 307 Temporary Redirect\r\nLocation: {location}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n"
        );
        stream
            .write_all(response.as_bytes())
            .expect("write redirect");
        request
    });
    (base_url, handle)
}

#[test]
fn endpoint_key_normalizes_endpoint_model_and_separates_capability() {
    let left = EndpointKey::new(
        "HTTP://LOCALHOST:8080/v1/",
        "Embedding-Model",
        "embedding",
        EndpointTransportPolicy::Normal,
    );
    let same = EndpointKey::new(
        "http://localhost:8080/v1",
        "embedding-model",
        "embedding",
        EndpointTransportPolicy::Normal,
    );
    let other_model = EndpointKey::new(
        "http://localhost:8080/v1",
        "chat-model",
        "embedding",
        EndpointTransportPolicy::Normal,
    );
    let other_capability = EndpointKey::new(
        "http://localhost:8080/v1",
        "embedding-model",
        "rerank",
        EndpointTransportPolicy::Normal,
    );
    let local_only = EndpointKey::new(
        "http://localhost:8080/v1",
        "embedding-model",
        "embedding",
        EndpointTransportPolicy::LocalOnly,
    );

    assert_eq!(left, same);
    assert_ne!(left, other_model);
    assert_ne!(left, other_capability);
    assert_ne!(left, local_only);
}

#[test]
fn endpoint_resource_name_uses_stable_redacted_fingerprint() {
    let name = endpoint_resource_name(
        "HTTP://LOCALHOST:8080/v1/",
        "Secret-Embedding-Model",
        "embedding",
    );
    let same = endpoint_resource_name(
        "http://localhost:8080/v1",
        "secret-embedding-model",
        "embedding",
    );
    let other_model =
        endpoint_resource_name("http://localhost:8080/v1", "other-model", "embedding");

    assert_eq!(name, same);
    assert_ne!(name, other_model);
    assert!(name.starts_with("model_endpoint:embedding:"));
    assert!(!name.contains("localhost"));
    assert!(!name.contains("8080"));
    assert!(!name.contains("Secret-Embedding-Model"));
    assert!(!name.contains("secret-embedding-model"));
}

#[test]
fn document_export_opt_in_keeps_remote_base_url_on_normal_transport() {
    let config = RerankConfig {
        allow_document_export: true,
        base_url: "https://rerank.example.test/v1".into(),
        ..Default::default()
    };
    let endpoint = OpenAiEndpoint::new_for_rerank(&config);

    assert!(!endpoint.local_only);
    assert_eq!(
        endpoint_url(&endpoint.base_url, "rerank", endpoint.local_only, "rerank")
            .expect("Normal transport permits a remote base_url"),
        "https://rerank.example.test/v1/rerank"
    );
}

#[tokio::test]
async fn local_only_llm_rerank_does_not_follow_cross_host_redirect() {
    let target = TcpListener::bind("127.0.0.1:0").expect("bind redirect target");
    let target_port = target.local_addr().expect("target addr").port();
    let redirect_location = format!("http://localhost:{target_port}/capture");
    let (base_url, redirect_handle) = spawn_redirect_server(redirect_location);
    let config = RerankConfig {
        enabled: true,
        strategy: RerankStrategy::Llm,
        provider: "openai_compatible".into(),
        base_url,
        model: "llm-reranker".into(),
        top_n: 1,
        timeout_seconds: 1,
        ..Default::default()
    };
    let reranker = OpenAiCompatibleLlmReranker::from_config(&config);
    let docs = vec!["candidate document must stay local".to_string()];

    let result = <OpenAiCompatibleLlmReranker as crate::traits::Reranker>::rerank_with_diagnostics(
        &reranker,
        "private query",
        &docs,
        1,
    )
    .await;
    let local_request = redirect_handle.join().expect("redirect server joins");
    target
        .set_nonblocking(true)
        .expect("set redirect target nonblocking");
    let remote_request = target
        .accept()
        .ok()
        .map(|(mut stream, _)| read_http_request(&mut stream));

    assert!(result.is_err(), "redirect response must not be followed");
    assert!(local_request.body.contains("private query"));
    assert!(local_request
        .body
        .contains("candidate document must stay local"));
    assert!(
        remote_request.is_none(),
        "cross-host target received rerank query or documents"
    );
}

fn assert_local_only_remote_endpoint_rejected(error: anyhow::Error, operation: &'static str) {
    let rerank_error = error
        .downcast_ref::<RerankError>()
        .expect("error carries rerank diagnostics");
    let provider_error = rerank_error
        .source_error()
        .downcast_ref::<ProviderError>()
        .expect("rerank error preserves provider error");

    assert!(matches!(
        provider_error,
        ProviderError::Configuration {
            operation: actual_operation,
            message,
        } if *actual_operation == operation
            && message == "LocalOnly transport requires a loopback or localhost base_url"
    ));
}

#[tokio::test]
async fn local_only_endpoint_reranker_rejects_remote_base_url_before_send() {
    let config = RerankConfig {
        enabled: true,
        allow_document_export: false,
        strategy: RerankStrategy::Endpoint,
        provider: "openai_compatible".into(),
        base_url: "https://rerank.example.test/v1".into(),
        model: "endpoint-reranker".into(),
        top_n: 1,
        timeout_seconds: 1,
        ..Default::default()
    };
    let reranker = OpenAiCompatibleReranker::from_config(&config);
    let docs = vec!["candidate document must not leave the host".to_string()];

    let error = <OpenAiCompatibleReranker as crate::traits::Reranker>::rerank_with_diagnostics(
        &reranker,
        "private query",
        &docs,
        1,
    )
    .await
    .expect_err("LocalOnly endpoint rerank rejects a remote base_url");

    assert_local_only_remote_endpoint_rejected(error, "rerank");
}

#[tokio::test]
async fn local_only_llm_reranker_rejects_remote_base_url_before_send() {
    let config = RerankConfig {
        enabled: true,
        allow_document_export: false,
        strategy: RerankStrategy::Llm,
        provider: "openai_compatible".into(),
        base_url: "https://rerank.example.test/v1".into(),
        model: "llm-reranker".into(),
        top_n: 1,
        timeout_seconds: 1,
        ..Default::default()
    };
    let reranker = OpenAiCompatibleLlmReranker::from_config(&config);
    let docs = vec!["candidate document must not leave the host".to_string()];

    let error = <OpenAiCompatibleLlmReranker as crate::traits::Reranker>::rerank_with_diagnostics(
        &reranker,
        "private query",
        &docs,
        1,
    )
    .await
    .expect_err("LocalOnly LLM rerank rejects a remote base_url");

    assert_local_only_remote_endpoint_rejected(error, "llm rerank");
}
