#[test]
fn issue_332_inspect_and_remove_keep_established_raw_source_routes() {
    let server = TestServer::respond_many([
        json_response(
            "200 OK",
            r#"{"id":"legacy-source-id","path":"/tmp/legacy","status":"Indexed","hash":"hash","parser_used":null,"last_ingested_at":null,"identity":{"kind":"source_record","schema_version":{"major":1,"minor":0,"patch":0},"artifact_id":"legacy-source-id","content_hash":"ea1492e2bb7df37b7034b549c3bf6c609cb01d377d5222df20d24f1bc5cc90ef"}}"#,
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
        "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{\"id\":\"opaque?query#fragment/segment\",\"path\":\"/srv/verbatim/renamed.md\",\"status\":\"Indexed\",\"hash\":\"hash-1\",\"parser_used\":\"markdown\",\"last_ingested_at\":\"now\",\"identity\":{\"kind\":\"source_record\",\"schema_version\":{\"major\":1,\"minor\":0,\"patch\":0},\"artifact_id\":\"opaque?query#fragment/segment\",\"content_hash\":\"107ebe3e8524173449da226d44089ff163542b7574bb87928b8339e28db777d4\"}}",
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
