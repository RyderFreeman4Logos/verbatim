use super::*;

#[test]
fn compact_retrieve_emits_persisted_source_hash() {
    assert_retrieve_source_hash(false);
}

#[test]
fn passage_retrieve_emits_persisted_source_hash() {
    assert_retrieve_source_hash(true);
}

#[test]
fn retrieve_source_lookup_count_is_constant_per_source() {
    const DISPLAYED_HITS: usize = 8;
    let (_dir, store, results) = source_hash_statement_count_fixture(DISPLAYED_HITS);

    for passage in [false, true] {
        let statement_count = |displayed_hits| {
            let mut input = retrieve_output_input(results[0].clone(), passage);
            input.results = results[..displayed_hits].to_vec();
            input.controls.limit = displayed_hits;
            input.controls.page_size = displayed_hits;
            refresh_final_evidence_pack_debug(&mut input.debug, &input.results);
            let (response, count) =
                store.count_sql_statements(|| final_retrieve_response(&store, input));
            assert_eq!(response.unwrap().results.len(), displayed_hits);
            count.unwrap()
        };

        assert_eq!(
            (statement_count(1), statement_count(DISPLAYED_HITS)),
            (5, 4 + DISPLAYED_HITS as u64),
            "passage={passage}: only evidence revalidation may grow after one source prefetch"
        );
    }
}

fn source_hash_statement_count_fixture(count: usize) -> (TestDir, Store, Vec<RetrievalResult>) {
    let (dir, store, first) = persisted_output_fixture("statement-count", None);
    let mut results = vec![first];
    for index in 1..count {
        let mut result = test_retrieval_result(
            index + 1,
            &format!("chunk-{}", index + 1),
            &format!("evidence-{index}"),
            EvidenceKind::Text,
        );
        let evidence = &mut result.evidence_units[0];
        evidence.text_hash = verbatim_core::types::hex_sha256(evidence.text.as_bytes());
        store
            .bulk_insert_evidence(std::slice::from_ref(evidence))
            .unwrap();
        store
            .bulk_insert_chunks(std::slice::from_ref(&result.chunk))
            .unwrap();
        store
            .link_chunk_evidence(&[(result.chunk.id.clone(), evidence.id.clone())])
            .unwrap();
        results.push(result);
    }
    drop(store);
    let store = Store::open_existing_readonly(&dir.path().join("verbatim.db")).unwrap();
    (dir, store, results)
}

fn assert_retrieve_source_hash(passage: bool) {
    let (_dir, store, persisted) =
        persisted_output_fixture(&format!("source-hash-{passage}"), None);
    let expected = persisted_source_hash(&store, &persisted.evidence_units[0].source_id).unwrap();

    let response =
        final_retrieve_response(&store, retrieve_output_input(persisted.clone(), passage))
            .expect("persisted source receipt resolves");
    let wire = serde_json::to_value(&response.results[0]).expect("retrieve result serializes");

    assert_eq!(wire["source_hash"], expected);
}

#[test]
fn retrieve_rejects_missing_persisted_source() {
    for passage in [false, true] {
        let (dir, store, persisted) =
            persisted_output_fixture(&format!("missing-source-{passage}"), None);
        delete_persisted_source(dir.path(), &persisted.evidence_units[0].source_id);

        let error =
            final_retrieve_response(&store, retrieve_output_input(persisted.clone(), passage))
                .expect_err("missing persisted source must fail the response");

        assert!(
            error.to_string().contains("persisted source not found"),
            "passage={passage}: {error}"
        );
    }
}

#[tokio::test]
async fn evidence_endpoint_emits_persisted_source_hash() {
    let (test_dir, store, persisted) = persisted_output_fixture("source-hash-route", None);
    let expected = persisted.evidence_units[0].clone();
    let expected_hash = persisted_source_hash(&store, &expected.source_id).unwrap();
    drop(store);
    let app = evidence_test_app(test_dir.path());

    let response = evidence_route_get(&app, &expected.id.0).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = evidence_route_body(response).await;
    let wire: serde_json::Value = serde_json::from_slice(&body).expect("valid evidence JSON");

    assert_eq!(wire["source_hash"], expected_hash);
}

#[tokio::test]
async fn evidence_endpoint_rejects_missing_persisted_source() {
    let (test_dir, store, persisted) = persisted_output_fixture("missing-source-route", None);
    let expected = persisted.evidence_units[0].clone();
    drop(store);
    let app = evidence_test_app(test_dir.path());
    delete_persisted_source(test_dir.path(), &expected.source_id);

    let response = evidence_route_get(&app, &expected.id.0).await;
    let status = response.status();
    let body = evidence_route_body(response).await;
    let body_text = String::from_utf8_lossy(&body);

    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body_text}");
    let error: ErrorResponse = serde_json::from_slice(&body).expect("typed evidence error");
    assert!(
        error.error.contains("persisted source not found"),
        "{error:?}"
    );
    assert!(!body_text.contains(&expected.text), "{body_text}");
}

#[tokio::test]
async fn generated_evidence_omits_source_hash() {
    let (test_dir, store, persisted) = persisted_output_fixture("generated-source-hash", None);
    let mut generated = persisted.evidence_units[0].clone();
    generated.id = EvidenceId("generated-source-hash".into());
    generated.kind = EvidenceKind::Generated;
    generated.derived_from = Some(persisted.evidence_units[0].id.clone());
    generated.text = "Generated caption from image.".into();
    generated.text_hash = verbatim_core::types::hex_sha256(generated.text.as_bytes());
    store
        .bulk_insert_evidence(std::slice::from_ref(&generated))
        .expect("generated evidence persists");
    drop(store);
    let app = evidence_test_app(test_dir.path());

    let response = evidence_route_get(&app, &generated.id.0).await;
    assert_eq!(response.status(), StatusCode::OK);
    let body = evidence_route_body(response).await;
    let wire: serde_json::Value = serde_json::from_slice(&body).expect("generated evidence JSON");

    assert_eq!(wire["source_bounded"], false);
    assert!(wire.get("source_hash").is_none(), "{wire}");
}

fn evidence_test_app(data_dir: &std::path::Path) -> Router {
    let config = retrieve_test_config("http://127.0.0.1:9/v1");
    let pipeline = IngestPipeline::new(&config, data_dir).unwrap();
    daemon_router(test_state(config, data_dir, pipeline))
}

fn delete_persisted_source(data_dir: &std::path::Path, source_id: &SourceId) {
    let connection = rusqlite::Connection::open(data_dir.join("verbatim.db")).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .unwrap();
    connection
        .execute("DELETE FROM sources WHERE id = ?1", [&source_id.0])
        .unwrap();
}
