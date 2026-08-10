use axum::body::{to_bytes, Body};
use axum::extract::connect_info::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;

use super::*;

const CORRUPTED_EVIDENCE_TEXT: &str = "Mutated persisted evidence must not escape.";

#[tokio::test]
async fn public_evidence_endpoint_revalidates_persisted_text() {
    let (test_dir, store, persisted) = persisted_output_fixture("public-endpoint", None);
    let expected = persisted.evidence_units[0].clone();
    drop(store);
    let config = retrieve_test_config("http://127.0.0.1:9/v1");
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let app = daemon_router(test_state(config, test_dir.path(), pipeline));

    let response = evidence_route_get(&app, &expected.id.0).await;
    assert_eq!(response.status(), StatusCode::OK);
    let returned: EvidenceResponse = serde_json::from_slice(&evidence_route_body(response).await)
        .expect("valid evidence response");
    assert_eq!(returned.id, expected.id.0);
    assert_eq!(returned.source_id, expected.source_id.0);
    assert!(returned.source_bounded);
    assert_eq!(returned.text_hash, expected.text_hash);
    assert_eq!(
        returned.text_hash,
        verbatim_core::types::hex_sha256(returned.text.as_bytes())
    );
    assert_eq!(returned.locator, expected.locator.to_string());
    assert_eq!(returned.structured_locator, expected.locator);
    assert_eq!(returned.text, expected.text);
    assert_eq!(returned.heading_path, expected.heading_path);
    assert_eq!(returned.position, expected.position);

    rusqlite::Connection::open(test_dir.path().join("verbatim.db"))
        .unwrap()
        .execute(
            "UPDATE evidence_units SET text = ?1 WHERE id = ?2",
            rusqlite::params![CORRUPTED_EVIDENCE_TEXT, &returned.id],
        )
        .unwrap();
    let response = evidence_route_get(&app, &returned.id).await;
    let status = response.status();
    let body = evidence_route_body(response).await;
    let body_text = String::from_utf8_lossy(&body);
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR, "{body_text}");
    assert!(!body_text.contains(CORRUPTED_EVIDENCE_TEXT), "{body_text}");
    let error: ErrorResponse = serde_json::from_slice(&body).expect("safe evidence error response");
    assert!(error.error.contains("text hash mismatch"), "{error:?}");

    let response = evidence_route_get(&app, "missing-evidence").await;
    assert_eq!(response.status(), StatusCode::NOT_FOUND);
}

async fn evidence_route_get(app: &Router, evidence_id: &str) -> Response {
    let mut request = Request::builder()
        .method(Method::GET)
        .uri(format!("/api/evidence/{evidence_id}"))
        .body(Body::empty())
        .unwrap();
    request.extensions_mut().insert(ConnectInfo(
        "127.0.0.1:43210".parse::<SocketAddr>().unwrap(),
    ));
    app.clone().oneshot(request).await.unwrap()
}

async fn evidence_route_body(response: Response) -> Vec<u8> {
    to_bytes(response.into_body(), 1024 * 1024)
        .await
        .unwrap()
        .to_vec()
}

#[test]
fn source_bounded_output_rehydrates_compact_evidence_from_store() {
    let (_dir, store, persisted) = persisted_output_fixture("compact", None);

    let valid = final_retrieve_response(&store, retrieve_output_input(persisted.clone(), false))
        .expect("persisted evidence should resolve");
    assert_source_bounded_result(&valid, &persisted.evidence_units[0]);
}

#[test]
fn source_bounded_output_rehydrates_passage_evidence_from_store() {
    let (_dir, store, persisted) = persisted_output_fixture("passage", None);

    let valid = final_retrieve_response(&store, retrieve_output_input(persisted.clone(), true))
        .expect("persisted evidence should resolve");
    assert_source_bounded_result(&valid, &persisted.evidence_units[0]);
}

#[test]
fn source_bounded_output_rejects_reindexed_evidence_after_retrieval_snapshot() {
    assert_reindexed_evidence_is_rejected(false);
    assert_reindexed_evidence_is_rejected(true);
}

#[test]
fn source_bounded_output_rejects_relocated_evidence_after_retrieval_snapshot() {
    assert_relocated_evidence_is_rejected(false);
    assert_relocated_evidence_is_rejected(true);
}

#[test]
fn source_bounded_output_rejects_unknown_evidence_id() {
    let (_dir, store, persisted) = persisted_output_fixture("unknown", None);
    let mut input = retrieve_output_input(persisted, false);
    input.debug.final_evidence_pack[0].evidence_id = EvidenceId("missing-evidence".into());

    let error = final_retrieve_response(&store, input).unwrap_err();

    assert!(error
        .to_string()
        .contains("source-bounded evidence not found"));
}

#[test]
fn source_bounded_output_rejects_persisted_text_hash_mismatch() {
    let (_dir, store, persisted) =
        persisted_output_fixture("hash-mismatch", Some("invalid-text-hash"));

    let error =
        final_retrieve_response(&store, retrieve_output_input(persisted, false)).unwrap_err();

    assert!(error.to_string().contains("text hash mismatch"));
}

fn assert_source_bounded_result(response: &RetrieveResponse, evidence: &EvidenceUnit) {
    assert!(response.source_bounded);
    assert_eq!(response.results[0].snippet, evidence.text);
    assert_eq!(response.results[0].locator, evidence.locator.to_string());
    assert_eq!(response.results[0].text_hash, evidence.text_hash);
}

fn final_retrieve_response(
    store: &Store,
    input: RetrieveResponseInput,
) -> Result<RetrieveResponse> {
    retrieve_response(store, input)
}

pub(super) fn persisted_retrieve_response(mut input: RetrieveResponseInput) -> RetrieveResponse {
    let store = Store::in_memory().unwrap();
    let mut source_ids = HashSet::new();
    let mut evidence_ids = HashSet::new();
    let mut evidence_units = Vec::new();
    for evidence in input
        .results
        .iter_mut()
        .flat_map(|result| &mut result.evidence_units)
    {
        evidence.text_hash = verbatim_core::types::hex_sha256(evidence.text.as_bytes());
        source_ids.insert(evidence.source_id.0.clone());
        if evidence_ids.insert(evidence.id.0.clone()) {
            evidence_units.push(evidence.clone());
        }
    }
    for source_id in source_ids {
        store
            .add_source(&Source {
                id: SourceId(source_id.clone()),
                path: PathBuf::from(format!("{source_id}.md")),
                hash: format!("hash-{source_id}"),
                status: SourceStatus::Indexed,
                parser_used: Some("markdown".into()),
                last_ingested_at: None,
            })
            .unwrap();
    }
    store.bulk_insert_evidence(&evidence_units).unwrap();
    retrieve_response(&store, input).unwrap()
}

fn persisted_output_fixture(
    name: &str,
    text_hash_override: Option<&str>,
) -> (TestDir, Store, RetrievalResult) {
    let dir = TestDir::new(&format!("source-bounded-output-{name}"));
    let store = Store::new(&dir.path().join("verbatim.db")).unwrap();
    let mut result = test_retrieval_result(1, "chunk-1", "ev-1", EvidenceKind::Text);
    let evidence = &mut result.evidence_units[0];
    evidence.locator = SourceLocator::Document {
        path_or_url: "persisted.md".into(),
        line_start: 7,
        line_end: Some(8),
    };
    evidence.text = "Persisted E1 text.".into();
    evidence.text_hash = text_hash_override.map_or_else(
        || verbatim_core::types::hex_sha256(evidence.text.as_bytes()),
        str::to_string,
    );
    result.chunk.text.clone_from(&evidence.text);
    store
        .add_source(&Source {
            id: evidence.source_id.clone(),
            path: dir.path().join("persisted.md"),
            hash: "source-hash".into(),
            status: SourceStatus::Indexed,
            parser_used: Some("markdown".into()),
            last_ingested_at: None,
        })
        .unwrap();
    store
        .bulk_insert_evidence(std::slice::from_ref(evidence))
        .unwrap();
    (dir, store, result)
}

fn retrieve_output_input(result: RetrievalResult, passage: bool) -> RetrieveResponseInput {
    let results = vec![result];
    let mut debug = empty_retrieval_debug();
    refresh_final_evidence_pack_debug(&mut debug, &results);
    RetrieveResponseInput {
        task_id: TaskId("task-source-bounded".into()),
        query: "What is cited?".into(),
        source_filter: Some(SourceId("src".into())),
        collection_filter: None,
        collection_provenance: HashMap::new(),
        embedding_profile_id: EmbeddingProfileId::default_profile(),
        controls: EffectiveRetrieveControls {
            limit: 1,
            page_size: 1,
            page: 1,
            include_debug: false,
            include_debug_packs: false,
            include_locator: true,
            passage,
            bypass_cache: false,
            fast: false,
            config: Config::default(),
            retrieval_config: RetrievalConfig::default(),
            rerank_config: RerankConfig::default(),
        },
        results,
        debug,
        source_paths: HashMap::new(),
        retrieval_ms: 1,
    }
}

fn assert_reindexed_evidence_is_rejected(passage: bool) {
    let (_dir, store, captured) = persisted_output_fixture("reindexed", None);
    let mut reindexed = captured.evidence_units[0].clone();
    reindexed.text = "Reindexed E1 text.".into();
    reindexed.text_hash = verbatim_core::types::hex_sha256(reindexed.text.as_bytes());
    reindexed.locator = SourceLocator::Document {
        path_or_url: "reindexed.md".into(),
        line_start: 21,
        line_end: Some(22),
    };

    replace_persisted_evidence(&store, &reindexed);

    let error = final_retrieve_response(&store, retrieve_output_input(captured, passage))
        .expect_err("reindexed evidence must not be paired with a retrieval snapshot");

    assert!(
        error.to_string().contains("changed since retrieval"),
        "{error}"
    );
}

fn assert_relocated_evidence_is_rejected(passage: bool) {
    let (_dir, store, captured) = persisted_output_fixture("relocated", None);
    let mut relocated = captured.evidence_units[0].clone();
    relocated.locator = SourceLocator::Document {
        path_or_url: "relocated.md".into(),
        line_start: 31,
        line_end: Some(32),
    };
    replace_persisted_evidence(&store, &relocated);

    let error = final_retrieve_response(&store, retrieve_output_input(captured, passage))
        .expect_err("relocated evidence must not be paired with a retrieval snapshot");

    assert!(
        error
            .to_string()
            .contains("identity changed since retrieval"),
        "{error}"
    );
}

fn replace_persisted_evidence(store: &Store, evidence: &EvidenceUnit) {
    store
        .remove_source_for_housekeeping(&evidence.source_id)
        .unwrap();
    store
        .add_source(&Source {
            id: evidence.source_id.clone(),
            path: PathBuf::from("reindexed.md"),
            hash: "reindexed-source-hash".into(),
            status: SourceStatus::Indexed,
            parser_used: Some("markdown".into()),
            last_ingested_at: None,
        })
        .unwrap();
    store
        .bulk_insert_evidence(std::slice::from_ref(evidence))
        .unwrap();
}
