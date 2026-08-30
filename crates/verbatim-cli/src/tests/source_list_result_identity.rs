fn source_list_http_response(sources: Vec<verbatim_core::api::SourceResponse>) -> String {
    let response = verbatim_core::SourceListResponse::new(sources).unwrap();
    json_response("200 OK", &serde_json::to_string(&response).unwrap())
}

fn source_response(id: &str) -> verbatim_core::api::SourceResponse {
    verbatim_core::api::SourceResponse::new(
        id,
        format!("/tmp/{id}.md"),
        "Ready",
        format!("hash-{id}"),
        Some("markdown".into()),
        None,
        None,
    )
    .unwrap()
}

#[test]
fn http_source_list_result_identity_decodes_and_validates() {
    let source = source_response("source-1");
    let server = TestServer::respond_many([source_list_http_response(vec![source.clone()])]);
    let client = HttpDaemonClient::with_base_url(server.base_url());

    let sources = client.list_sources().unwrap();

    assert_eq!(sources, vec![source.clone()]);
    assert!(server.request().starts_with("GET /api/sources HTTP/1.1"));

    let server = TestServer::respond_many([json_response(
        "200 OK",
        &serde_json::to_string(&vec![source.clone()]).unwrap(),
    )]);
    let client = HttpDaemonClient::with_base_url(server.base_url());
    assert!(client.list_sources().is_err());
    let _ = server.request();

    let response = verbatim_core::SourceListResponse::new(vec![source]).unwrap();
    let mut stale = serde_json::to_value(response).unwrap();
    stale["sources"][0] = serde_json::to_value(source_response("source-2")).unwrap();
    let server = TestServer::respond_many([json_response("200 OK", &stale.to_string())]);
    let client = HttpDaemonClient::with_base_url(server.base_url());
    assert!(client.list_sources().is_err());
    let _ = server.request();
}
