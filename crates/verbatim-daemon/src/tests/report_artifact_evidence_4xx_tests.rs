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
