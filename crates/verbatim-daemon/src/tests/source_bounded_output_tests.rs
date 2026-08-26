use axum::body::{to_bytes, Body};
use axum::extract::connect_info::ConnectInfo;
use axum::http::{Method, Request, StatusCode};
use tower::ServiceExt;

use super::*;
use crate::source_bounded_retrieval::filter_generated_retrieval_evidence;

const CORRUPTED_EVIDENCE_TEXT: &str = "Mutated persisted evidence must not escape.";

#[path = "audit_receipt_tests.rs"]
mod audit_receipt_tests;
#[path = "canonical_fixture_passage_tests.rs"]
mod canonical_fixture_passage_tests;
#[path = "default_retrieval_scope_tests.rs"]
mod default_retrieval_scope_tests;
#[path = "source_bounded_batch_tests.rs"]
mod source_bounded_batch_tests;
#[path = "source_hash_receipt_tests.rs"]
mod source_hash_receipt_tests;

fn write_pdf_with_text_and_image(path: &std::path::Path) {
    let image = vec![255_u8; 8 * 8 * 3];
    let content = b"BT\n/F1 12 Tf\n72 120 Td\n(Source-backed predecessor text.) Tj\nET\nq\n36 0 0 36 72 72 cm\n/Im1 Do\nQ\n";
    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 4 0 R >> /XObject << /Im1 5 0 R >> >> /Contents 6 0 R >>".to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        pdf_stream_object(
            b"<< /Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceRGB /BitsPerComponent 8",
            &image,
        ),
        pdf_stream_object(b"<<", content),
    ];
    std::fs::write(path, pdf_bytes(objects)).expect("PDF image fixture writes");
}

fn pdf_stream_object(prefix: &[u8], data: &[u8]) -> Vec<u8> {
    let mut object = prefix.to_vec();
    object.extend(format!(" /Length {} >>\nstream\n", data.len()).as_bytes());
    object.extend(data);
    object.extend(b"\nendstream");
    object
}

fn pdf_bytes(objects: Vec<Vec<u8>>) -> Vec<u8> {
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend(object);
        pdf.extend(b"\nendobj\n");
    }
    let xref_offset = pdf.len();
    pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        pdf.extend(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    pdf
}

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

#[tokio::test]
async fn public_generated_evidence_is_not_source_bounded() {
    let (test_dir, store, persisted) = persisted_output_fixture("public-generated", None);
    let source = persisted.evidence_units[0].clone();
    let mut generated = source.clone();
    generated.id = EvidenceId("generated-caption".into());
    generated.kind = EvidenceKind::Generated;
    generated.derived_from = Some(source.id.clone());
    generated.text = "Generated caption from image.".into();
    generated.text_hash = verbatim_core::types::hex_sha256(generated.text.as_bytes());
    let mut ocr = source.clone();
    ocr.id = EvidenceId("ocr-control".into());
    ocr.kind = EvidenceKind::Ocr;
    ocr.text = "OCR control text.".into();
    ocr.text_hash = verbatim_core::types::hex_sha256(ocr.text.as_bytes());
    store
        .bulk_insert_evidence(&[generated.clone(), ocr.clone()])
        .expect("generated and OCR evidence persist");
    drop(store);

    let config = retrieve_test_config("http://127.0.0.1:9/v1");
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let app = daemon_router(test_state(config, test_dir.path(), pipeline));

    let returned: EvidenceResponse = serde_json::from_slice(
        &evidence_route_body(evidence_route_get(&app, &generated.id.0).await).await,
    )
    .expect("generated evidence response");
    assert_eq!(returned.id, generated.id.0);
    assert!(!returned.source_bounded);
    assert_eq!(returned.text_hash, generated.text_hash);
    assert_eq!(returned.text, generated.text);

    let returned: EvidenceResponse = serde_json::from_slice(
        &evidence_route_body(evidence_route_get(&app, &ocr.id.0).await).await,
    )
    .expect("OCR evidence response");
    assert_eq!(returned.id, ocr.id.0);
    assert!(!returned.source_bounded);
    assert_eq!(returned.text_hash, ocr.text_hash);
    assert_eq!(returned.text, ocr.text);
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
fn source_bounded_retrieval_omits_generated_captions_from_all_response_forms() {
    for passage in [false, true] {
        for include_debug in [false, true] {
            let (_dir, store, source) = persisted_output_fixture("captions", None);
            let derived = generated_caption_result(
                2,
                "caption-derived",
                "caption-derived-id",
                "generated caption derived from an image",
                Some(source.evidence_units[0].id.clone()),
            );
            let standalone = generated_caption_result(
                3,
                "caption-standalone",
                "caption-standalone-id",
                "standalone generated caption",
                None,
            );
            store
                .bulk_insert_evidence(&derived.evidence_units)
                .expect("derived caption persists");
            store
                .bulk_insert_evidence(&standalone.evidence_units)
                .expect("standalone caption persists");

            let mut input = retrieve_output_input(source.clone(), passage);
            input.controls.limit = 3;
            input.controls.page_size = 3;
            input.controls.include_debug = include_debug;
            input.controls.include_debug_packs = include_debug;
            input.results.extend([derived, standalone]);
            refresh_final_evidence_pack_debug(&mut input.debug, &input.results);

            let response = final_retrieve_response(&store, input).expect("source output resolves");
            let serialized = serde_json::to_string(&response).expect("response serializes");

            assert_eq!(
                response.total_results, 1,
                "passage={passage}, debug={include_debug}"
            );
            assert_eq!(
                response.returned_results, 1,
                "passage={passage}, debug={include_debug}"
            );
            assert_eq!(
                response.results[0].index, 0,
                "passage={passage}, debug={include_debug}"
            );
            assert_eq!(
                response.results[0].rank, 1,
                "passage={passage}, debug={include_debug}"
            );
            assert_eq!(
                response.results[0].label, "E1",
                "passage={passage}, debug={include_debug}"
            );
            for generated in [
                "caption-derived-id",
                "caption-standalone-id",
                "generated caption derived from an image",
                "standalone generated caption",
                "image_caption_generated",
                "generated",
            ] {
                assert!(
                    !serialized.contains(generated),
                    "generated retrieval data leaked for passage={passage}, debug={include_debug}: {generated}"
                );
            }
        }
    }
}

#[tokio::test]
async fn source_bounded_retrieval_task_telemetry_uses_filtered_evidence_snapshot() {
    let test_dir = TestDir::new("source-bounded-task-telemetry");
    let mut source =
        test_retrieval_result(1, "source-chunk", "source-evidence", EvidenceKind::Text);
    source.evidence_units[0].text = "source-boundary-needle source evidence".into();
    source.evidence_units[0].text_hash =
        verbatim_core::types::hex_sha256(source.evidence_units[0].text.as_bytes());
    source.chunk.text.clone_from(&source.evidence_units[0].text);
    let derived = generated_caption_result(
        2,
        "derived-chunk",
        "derived-caption",
        "source-boundary-needle generated caption derived from source",
        Some(source.evidence_units[0].id.clone()),
    );
    let standalone = generated_caption_result(
        3,
        "standalone-chunk",
        "standalone-caption",
        "source-boundary-needle generated standalone caption",
        None,
    );
    let results = vec![source, derived, standalone];
    let store = Store::new(&test_dir.path().join("verbatim.db")).unwrap();
    store
        .add_source(&Source {
            id: SourceId("src".into()),
            path: test_dir.path().join("source.md"),
            hash: "source-hash".into(),
            status: SourceStatus::Indexed,
            parser_used: Some("plaintext".into()),
            last_ingested_at: None,
        })
        .unwrap();
    let evidence = results
        .iter()
        .flat_map(|result| result.evidence_units.clone())
        .collect::<Vec<_>>();
    let chunks = results
        .iter()
        .map(|result| result.chunk.clone())
        .collect::<Vec<_>>();
    let links = chunks
        .iter()
        .zip(&evidence)
        .map(|(chunk, evidence)| (chunk.id.clone(), evidence.id.clone()))
        .collect::<Vec<_>>();
    store.bulk_insert_evidence(&evidence).unwrap();
    store.bulk_insert_chunks(&chunks).unwrap();
    store.link_chunk_evidence(&links).unwrap();
    drop(store);

    let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
    config.embedding.enabled = false;
    config.rerank.enabled = false;
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    pipeline.fts_startup_maintenance();
    let state = test_state(config, test_dir.path(), pipeline);
    let response = retrieve(
        State(Arc::clone(&state)),
        Json(RetrieveRequest {
            question: "source-boundary-needle".into(),
            source_id: None,
            collection_filter: CollectionFilterRequest::default(),
            embedding_profile_id: None,
            limit: Some(3),
            page_size: Some(3),
            page: Some(1),
            fast: true,
            rerank: Some(false),
            dense_top_k: None,
            bm25_top_k: Some(3),
            rerank_top_n: None,
            bypass_cache: false,
            include_debug: true,
            include_debug_packs: true,
            include_locator: false,
            passage: false,
        }),
    )
    .await
    .unwrap()
    .0;

    assert_eq!(response.total_results, 1);
    assert_eq!(response.returned_results, 1);
    let debug = response.debug.expect("retrieval debug");
    assert_eq!(debug.final_evidence_count, 1);
    assert_eq!(debug.display_evidence_count, 1);

    let task_id = TaskId(response.task_id);
    let summary = task_summary_response(&state, task_id.clone())
        .await
        .unwrap();
    let progress = summary.task.progress.as_ref().expect("retrieval progress");
    assert_eq!(
        progress
            .counters
            .iter()
            .find(|counter| counter.name == "evidence")
            .expect("evidence progress counter")
            .completed,
        1
    );
    let retrieval_span = summary
        .spans
        .iter()
        .find(|span| span.phase == "retrieval")
        .expect("retrieval task span");
    assert_eq!(retrieval_span.metadata["result_count"], 1);
    assert_eq!(retrieval_span.metadata["returned_results"], 1);

    let profile = task_profile_response(&state, task_id)
        .await
        .unwrap()
        .profile
        .retrieve
        .expect("retrieve task profile");
    assert_eq!(profile.evidence.result_count, 1);
    assert_eq!(profile.evidence.final_count, 1);
    assert_eq!(profile.evidence.display_count, 1);
    assert_eq!(profile.display.returned_count, 1);
}

#[tokio::test]
async fn source_bounded_retrieval_debug_omits_production_caption_chunk_identities() {
    let model_server = MockModelServer::start_with_chat(
        3,
        r#"{
  "type": "diagram",
  "short_caption": "A captiondebugneedle indexing diagram.",
  "detailed_description": "An input flows into an index.",
  "visible_text": ["Input", "Index"],
  "key_entities": ["Input", "Index"],
  "relationships": [{"from": "Input", "to": "Index", "label": "feeds"}],
  "answerable_questions": ["What feeds the index?"],
  "uncertainties": []
}"#,
    )
    .await;
    let test_dir = TestDir::new("source-bounded-production-caption-debug");
    let pdf_path = test_dir.path().join("caption.pdf");
    write_pdf_with_text_and_image(&pdf_path);

    let mut config = retrieve_test_config(&model_server.base_url);
    config.embedding.enabled = false;
    config.vision.enabled = true;
    config.vision.base_url.clone_from(&model_server.base_url);
    config.vision.model = "test-vision".into();
    let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let source_id = pipeline.add_source(&pdf_path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();

    let (generated_id, caption_chunk_id) = source_bounded_batch_tests::persist_legacy_caption(
        &pipeline,
        &source_id,
        "captiondebugneedle",
        "legacy-caption-debug",
        "legacy-caption-debug-chunk",
        "captiondebugneedle legacy generated caption",
    );

    let state = test_state(config, test_dir.path(), pipeline);
    let response = retrieve(
        State(Arc::clone(&state)),
        Json(RetrieveRequest {
            question: "captiondebugneedle".into(),
            source_id: Some(source_id.0.clone()),
            collection_filter: CollectionFilterRequest::default(),
            embedding_profile_id: None,
            limit: Some(3),
            page_size: Some(3),
            page: Some(1),
            fast: true,
            rerank: Some(false),
            dense_top_k: None,
            bm25_top_k: Some(3),
            rerank_top_n: None,
            bypass_cache: false,
            include_debug: true,
            include_debug_packs: true,
            include_locator: false,
            passage: false,
        }),
    )
    .await
    .unwrap()
    .0;

    let serialized = serde_json::to_string(&response).expect("response serializes");
    assert!(response.results.is_empty(), "{serialized}");
    assert!(!serialized.contains(&generated_id.0), "{serialized}");
    assert!(!serialized.contains(&caption_chunk_id.0), "{serialized}");
}

#[tokio::test]
async fn source_bounded_retrieval_no_debug_omits_production_caption_seed_identity() {
    let model_server = MockModelServer::start_with_chat(
        3,
        r#"{
  "type": "diagram",
  "short_caption": "A captionseedneedle indexing diagram.",
  "detailed_description": "An input flows into an index.",
  "visible_text": ["Input", "Index"],
  "key_entities": ["Input", "Index"],
  "relationships": [{"from": "Input", "to": "Index", "label": "feeds"}],
  "answerable_questions": ["What feeds the index?"],
  "uncertainties": []
}"#,
    )
    .await;
    let test_dir = TestDir::new("source-bounded-production-caption-seed");
    let pdf_path = test_dir.path().join("caption.pdf");
    write_pdf_with_text_and_image(&pdf_path);

    let mut config = retrieve_test_config(&model_server.base_url);
    config.embedding.enabled = false;
    config.rerank.enabled = false;
    config.vision.enabled = true;
    config.vision.base_url.clone_from(&model_server.base_url);
    config.vision.model = "test-vision".into();
    let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let source_id = pipeline.add_source(&pdf_path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();

    let (generated_id, caption_chunk_id) = source_bounded_batch_tests::persist_legacy_caption(
        &pipeline,
        &source_id,
        "captionseedneedle",
        "legacy-caption-seed",
        "legacy-caption-seed-chunk",
        "captionseedneedle legacy generated caption",
    );

    let state = test_state(config, test_dir.path(), pipeline);
    let response = retrieve(
        State(Arc::clone(&state)),
        Json(RetrieveRequest {
            question: "captionseedneedle".into(),
            source_id: Some(source_id.0.clone()),
            collection_filter: CollectionFilterRequest::default(),
            embedding_profile_id: None,
            limit: Some(3),
            page_size: Some(3),
            page: Some(1),
            fast: true,
            rerank: Some(false),
            dense_top_k: None,
            bm25_top_k: Some(3),
            rerank_top_n: None,
            bypass_cache: false,
            include_debug: false,
            include_debug_packs: false,
            include_locator: true,
            passage: false,
        }),
    )
    .await
    .unwrap()
    .0;

    assert!(response.debug.is_none());
    let serialized = serde_json::to_string(&response).expect("response serializes");
    assert!(response.results.is_empty(), "{serialized}");
    assert!(!serialized.contains(&caption_chunk_id.0), "{serialized}");
    assert!(!serialized.contains(&generated_id.0), "{serialized}");
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
    mut input: RetrieveResponseInput,
) -> Result<RetrieveResponse> {
    filter_generated_retrieval_evidence(
        store,
        &input.embedding_profile_id,
        &mut input.results,
        Some(&mut input.debug),
        input.controls.include_debug,
    )?;
    input.sources = sources_for_results(&input.results, store)?;
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
    input.sources = sources_for_results(&input.results, &store).unwrap();
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
    store
        .bulk_insert_chunks(std::slice::from_ref(&result.chunk))
        .unwrap();
    store
        .link_chunk_evidence(&[(result.chunk.id.clone(), result.evidence_units[0].id.clone())])
        .unwrap();
    (dir, store, result)
}

fn generated_caption_result(
    rank: usize,
    chunk_id: &str,
    evidence_id: &str,
    text: &str,
    derived_from: Option<EvidenceId>,
) -> RetrievalResult {
    let mut result = test_retrieval_result(rank, chunk_id, evidence_id, EvidenceKind::Generated);
    let evidence = &mut result.evidence_units[0];
    evidence.text = text.into();
    evidence.text_hash = verbatim_core::types::hex_sha256(evidence.text.as_bytes());
    evidence.derived_from = derived_from;
    result.chunk.text.clone_from(&evidence.text);
    result
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
        sources: HashMap::new(),
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

    replace_persisted_evidence(&store, &reindexed, &captured.chunk);

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
    replace_persisted_evidence(&store, &relocated, &captured.chunk);

    let error = final_retrieve_response(&store, retrieve_output_input(captured, passage))
        .expect_err("relocated evidence must not be paired with a retrieval snapshot");

    assert!(
        error
            .to_string()
            .contains("identity changed since retrieval"),
        "{error}"
    );
}

fn replace_persisted_evidence(store: &Store, evidence: &EvidenceUnit, chunk: &Chunk) {
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
    store
        .bulk_insert_chunks(std::slice::from_ref(chunk))
        .unwrap();
    store
        .link_chunk_evidence(&[(chunk.id.clone(), evidence.id.clone())])
        .unwrap();
}
