#[test]
fn issue_332_inspect_and_remove_keep_established_raw_source_routes() {
    let server = TestServer::respond_many([
        json_response(
            "200 OK",
            r#"{"id":"legacy-source-id","path":"/tmp/legacy","status":"Indexed","hash":"hash","parser_used":null,"last_ingested_at":null}"#,
        ),
        "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_string(),
    ]);
    let client = HttpDaemonClient::with_base_url(server.base_url());

    assert_eq!(client.get_source("legacy-source-id").unwrap().id, "legacy-source-id");
    client.remove_source("legacy-source-id").unwrap();

    let requests = server.requests();
    assert!(requests[0].starts_with("GET /api/sources/legacy-source-id HTTP/1.1"));
    assert!(requests[1].starts_with("DELETE /api/sources/legacy-source-id HTTP/1.1"));
}

#[test]
fn issue_332_http_relocate_carries_opaque_source_id_in_json_body() {
    let server = TestServer::respond_once(
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"id\":\"opaque?query#fragment/segment\",\"path\":\"/srv/verbatim/renamed.md\",\"status\":\"Indexed\",\"hash\":\"hash-1\",\"parser_used\":\"markdown\",\"last_ingested_at\":\"now\"}",
    );
    let client = HttpDaemonClient::with_base_url(server.base_url());

    let source = client
        .relocate_source(
            "opaque?query#fragment/segment",
            "/srv/verbatim/renamed.md",
        )
        .unwrap();

    assert_eq!(source.id, "opaque?query#fragment/segment");
    assert_eq!(source.path, "/srv/verbatim/renamed.md");
    let request = server.request();
    assert!(request.starts_with("POST /api/source-relocations HTTP/1.1"));
    assert!(request.contains("\"source_id\":\"opaque?query#fragment/segment\""));
    assert!(request.contains("\"new_path\":\"/srv/verbatim/renamed.md\""));
}
