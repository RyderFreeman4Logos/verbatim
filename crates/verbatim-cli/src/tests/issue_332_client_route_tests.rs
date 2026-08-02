#[test]
fn issue_332_source_id_segment_protocol_covers_opaque_ids() {
    for (source_id, expected) in [
        (".", "~."),
        ("..", "~.."),
        ("%", "~%25"),
        ("雪", "~%E9%9B%AA"),
        ("/", "~%2F"),
        ("?", "~%3F"),
        ("#", "~%23"),
        ("~prefixed", "~~prefixed"),
    ] {
        assert_eq!(encode_source_id_segment(source_id), expected);
    }
}

#[test]
fn issue_332_dot_source_ids_are_not_url_normalized() {
    let server = TestServer::respond_many([
        json_response(
            "200 OK",
            r#"{"id":".","path":"/tmp/dot","status":"Indexed","hash":"hash","parser_used":null,"last_ingested_at":null}"#,
        ),
        json_response(
            "200 OK",
            r#"{"id":"..","path":"/tmp/dotdot","status":"Indexed","hash":"hash","parser_used":null,"last_ingested_at":null}"#,
        ),
    ]);
    let client = HttpDaemonClient::with_base_url(server.base_url());

    assert_eq!(client.get_source(".").unwrap().id, ".");
    assert_eq!(client.get_source("..").unwrap().id, "..");

    let requests = server.requests();
    assert!(requests[0].starts_with("GET /api/sources/~. HTTP/1.1"));
    assert!(requests[1].starts_with("GET /api/sources/~.. HTTP/1.1"));
}

#[test]
fn issue_332_inspect_and_remove_encode_opaque_source_id() {
    let server = TestServer::respond_many([
        json_response(
            "200 OK",
            r#"{"id":"legacy?part/#","path":"/tmp/legacy","status":"Indexed","hash":"hash","parser_used":null,"last_ingested_at":null}"#,
        ),
        "HTTP/1.1 204 No Content\r\nConnection: close\r\n\r\n".to_string(),
    ]);
    let client = HttpDaemonClient::with_base_url(server.base_url());

    assert_eq!(
        client.get_source("legacy?part/#").unwrap().id,
        "legacy?part/#"
    );
    client.remove_source("legacy?part/#").unwrap();

    let requests = server.requests();
    assert!(requests[0].starts_with("GET /api/sources/~legacy%3Fpart%2F%23 HTTP/1.1"));
    assert!(requests[1].starts_with("DELETE /api/sources/~legacy%3Fpart%2F%23 HTTP/1.1"));
}

#[test]
fn issue_332_http_relocate_encodes_opaque_source_id_as_one_path_segment() {
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
    assert!(request.starts_with(
        "POST /api/sources/~opaque%3Fquery%23fragment%2Fsegment/relocate HTTP/1.1"
    ));
    assert!(request.contains("\"new_path\":\"/srv/verbatim/renamed.md\""));
}
