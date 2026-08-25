use super::*;

#[tokio::test]
async fn reserved_report_artifact_ids_return_typed_client_error() {
    let (test_dir, store, persisted) = persisted_output_fixture("report-artifact-4xx", None);
    let ordinary = persisted.evidence_units[0].clone();
    drop(store);
    let config = retrieve_test_config("http://127.0.0.1:9/v1");
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let state = test_state(config, test_dir.path(), pipeline);

    let ok = get_evidence(State(state.clone()), Path(ordinary.id.0.clone()))
        .await
        .expect("ordinary evidence stays 200");
    assert_eq!(ok.0.id, ordinary.id.0);

    let missing = get_evidence(State(state.clone()), Path("missing-evidence".into()))
        .await
        .expect_err("missing ordinary evidence stays 404");
    assert_eq!(missing.0, StatusCode::NOT_FOUND);

    for id in [
        "graphrag://report/community-test",
        "graphrag:report:community-test",
    ] {
        let (status, Json(error)) = get_evidence(State(state.clone()), Path(id.into()))
            .await
            .expect_err("reserved report-artifact id must not resolve as evidence");
        assert!(
            status.is_client_error(),
            "reserved report-artifact id must be 4xx, got {status}: {}",
            error.error
        );
        assert_ne!(
            status,
            StatusCode::NOT_FOUND,
            "reserved report-artifact id is not missing evidence: {}",
            error.error
        );
        assert!(
            error.error.contains("report artifact"),
            "4xx body must say these IDs are report artifacts: {error:?}"
        );
        assert!(
            error.error.contains("not evidence"),
            "4xx body must say these IDs are not evidence: {error:?}"
        );
    }
}

#[tokio::test]
async fn canonical_report_artifact_id_is_reachable_on_real_router() {
    let (test_dir, store, _persisted) = persisted_output_fixture("report-artifact-router", None);
    drop(store);
    let app = evidence_test_app(test_dir.path());

    let response = evidence_route_get(&app, "graphrag://report/community-test").await;
    let status = response.status();
    let body = evidence_route_body(response).await;
    let body_text = String::from_utf8_lossy(&body);
    let error: ErrorResponse = serde_json::from_slice(&body).unwrap_or_else(|_| {
        panic!("canonical report-artifact id must reach get_evidence as JSON 4xx, got {status}: {body_text}")
    });
    assert!(
        status.is_client_error(),
        "canonical report-artifact id must be 4xx on the real Router, got {status}: {}",
        error.error
    );
    assert_ne!(
        status,
        StatusCode::NOT_FOUND,
        "canonical report-artifact id must not 404 on extra '/': {}",
        error.error
    );
    assert!(
        error.error.contains("report artifact"),
        "4xx body must say these IDs are report artifacts: {error:?}"
    );
    assert!(
        error.error.contains("not evidence"),
        "4xx body must say these IDs are not evidence: {error:?}"
    );
}

#[tokio::test]
async fn reserved_report_artifact_ids_are_rejected_before_store_io() {
    let (test_dir, store, persisted) = persisted_output_fixture("report-artifact-before-io", None);
    let ordinary = persisted.evidence_units[0].clone();
    drop(store);
    let config = retrieve_test_config("http://127.0.0.1:9/v1");
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let state = test_state(config, test_dir.path(), pipeline);
    std::fs::remove_file(test_dir.path().join("verbatim.db")).expect("unlink store before lookup");

    let (status, Json(error)) = get_evidence(
        State(state.clone()),
        Path("graphrag://report/community-test".into()),
    )
    .await
    .expect_err("reserved id must 400 before store IO");
    assert_eq!(status, StatusCode::BAD_REQUEST);
    assert!(
        error.error.contains("report artifact"),
        "reserved-id 400 must not be a rewritten store failure: {error:?}"
    );
    assert!(
        error.error.contains("not evidence"),
        "reserved-id 400 must not be a rewritten store failure: {error:?}"
    );

    let (status, Json(error)) = get_evidence(State(state), Path(ordinary.id.0))
        .await
        .expect_err("unrelated store IO failure stays 500");
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    assert!(
        !error.error.contains("report artifact"),
        "store IO failure must not be rewritten as reserved-id 400: {error:?}"
    );
}
