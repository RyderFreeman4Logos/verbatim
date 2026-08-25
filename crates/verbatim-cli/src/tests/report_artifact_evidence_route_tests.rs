#[test]
fn http_get_evidence_encodes_canonical_report_artifact_id() {
    let server = TestServer::respond_many([json_response(
        "400 Bad Request",
        r#"{"error":"report artifact ids are not evidence: graphrag://report/community-test"}"#,
    )]);
    let client = HttpDaemonClient::with_base_url(server.base_url());

    let error = client
        .get_evidence("graphrag://report/community-test")
        .unwrap_err();
    assert!(error.to_string().contains("report artifact"), "{}", error);
    assert!(error.to_string().contains("not evidence"), "{}", error);

    let request = server.request();
    let request_line = request.lines().next().expect("HTTP request line");
    assert_eq!(
        request_line,
        "GET /api/evidence/graphrag%3A%2F%2Freport%2Fcommunity-test HTTP/1.1"
    );
}
