#[test]
fn http_get_report_artifact_lookup_encodes_reserved_ids() {
    let server = TestServer::respond_many([
        json_response(
            "404 Not Found",
            r#"{"error":"report artifact not found: graphrag://report/community-test","code":"report_artifact_not_found"}"#,
        ),
        json_response(
            "404 Not Found",
            r#"{"error":"report artifact not found: graphrag:report:community-test","code":"report_artifact_not_found"}"#,
        ),
    ]);
    let client = HttpDaemonClient::with_base_url(server.base_url());

    let canonical = client
        .get_report_artifact("graphrag://report/community-test")
        .unwrap_err();
    assert!(
        canonical.to_string().contains("report artifact"),
        "{canonical}"
    );

    let legacy = client
        .get_report_artifact("graphrag:report:community-test")
        .unwrap_err();
    assert!(legacy.to_string().contains("report artifact"), "{legacy}");

    let requests = server.requests();
    assert_eq!(
        requests[0].lines().next().expect("canonical request line"),
        "GET /api/report-artifact/graphrag%3A%2F%2Freport%2Fcommunity-test HTTP/1.1"
    );
    assert_eq!(
        requests[1].lines().next().expect("legacy request line"),
        "GET /api/report-artifact/graphrag%3Areport%3Acommunity-test HTTP/1.1"
    );
}
