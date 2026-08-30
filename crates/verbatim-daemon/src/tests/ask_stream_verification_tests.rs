use axum::body::{to_bytes, Body};
use axum::http::{Request, StatusCode};
use tower::ServiceExt;
use verbatim_core::wire_schemas::{decode_context_pack_envelope_json, WIRE_SCHEMA_VERSION};

use super::*;

async fn ask_stream_body(
    name: &str,
    verifier_enabled: bool,
    chat_responses: &[&str],
    fail_success_persistence: bool,
) -> (String, MockModelServer) {
    let model_server =
        MockModelServer::start_with_chat_responses(3, chat_responses.iter().copied()).await;
    let test_dir = TestDir::new(name);
    let source_path = test_dir.path().join("doc.md");
    fs::write(
        &source_path,
        "The stored evidence says the safe answer is alpha.",
    )
    .unwrap();
    let mut config = retrieve_test_config(&model_server.base_url);
    config.embedding.enabled = false;
    config.chat.enabled = true;
    config.chat.base_url = model_server.base_url.clone();
    config.chat.model = "test-chat".into();
    config.rerank.enabled = false;
    config.verifier.enabled = verifier_enabled;
    let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let source_id = pipeline.add_source(&source_path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();
    let state = test_state(config, test_dir.path(), pipeline);
    state.fail_next_task_success_persistence.store(
        fail_success_persistence,
        std::sync::atomic::Ordering::Release,
    );
    let app = Router::new()
        .route("/api/ask/stream", post(ask_stream))
        .with_state(state);
    let request = Request::builder()
        .method("POST")
        .uri("/api/ask/stream")
        .header("content-type", "application/json")
        .body(Body::from(
            serde_json::to_vec(&AskRequest {
                question: "What does the stored evidence say?".into(),
                source_id: Some(source_id.0),
                collection_filter: CollectionFilterRequest::default(),
                embedding_profile_id: None,
                show_retrieval: false,
                context_only: false,
                limit: None,
                page_size: None,
                page: None,
            })
            .unwrap(),
        ))
        .unwrap();

    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()["content-type"], "text/event-stream");
    let body = to_bytes(response.into_body(), 1024 * 1024).await.unwrap();

    (String::from_utf8(body.to_vec()).unwrap(), model_server)
}

fn event_data<'a>(body: &'a str, event: &str) -> Vec<&'a str> {
    body.split("\n\n")
        .filter_map(|frame| {
            let (event_line, data_line) = frame.split_once('\n')?;
            (event_line.strip_prefix("event: ")? == event)
                .then(|| data_line.strip_prefix("data: "))
                .flatten()
        })
        .collect()
}

fn report_citation() -> CitationRef {
    CitationRef {
        label: "E1".into(),
        evidence_id: EvidenceId("graphrag://report/community-1".into()),
        backing_evidence_id: Some(EvidenceId("ev-1".into())),
        source_id: SourceId("src-1".into()),
        kind: EvidenceKind::Text,
        role: RetrievalEvidenceRole::GraphReport,
        derived_from: None,
        locator: SourceLocator::Document {
            path_or_url: "report.md".into(),
            line_start: 1,
            line_end: Some(1),
        },
        text_preview: "Report claim".into(),
    }
}

#[test]
fn sync_ask_citation_publishes_report_artifact_identity() {
    let response = citation_response_with_collections(report_citation(), &HashMap::new());

    assert_eq!(response.evidence_id, "graphrag://report/community-1");
}

#[test]
fn sse_citation_event_publishes_report_artifact_identity() {
    let event = AskCitationEvent::new(
        vec![citation_response_with_collections(
            report_citation(),
            &HashMap::new(),
        )],
        true,
    )
    .unwrap();
    let encoded = serde_json::to_value(event).unwrap();

    assert_eq!(
        encoded["citations"][0]["evidence_id"],
        "graphrag://report/community-1"
    );
}

#[tokio::test]
async fn ask_stream_with_verifier_publishes_only_one_generated_interpretation_answer() {
    let raw_draft = "The safe answer is alpha [E1].";
    let (body, model_server) = ask_stream_body(
        "ask-stream-verifier-pass",
        true,
        &[raw_draft, r#"{"verdict":"pass","unsupported_claims":[]}"#],
        false,
    )
    .await;

    assert!(event_data(&body, "token").is_empty(), "SSE: {body}");
    let answers = event_data(&body, "answer");
    assert_eq!(answers.len(), 1, "SSE: {body}");
    let answer: AskResponse = serde_json::from_str(answers[0]).unwrap();
    assert!(answer.answer.starts_with(raw_draft));
    assert_eq!(
        answer.generated_interpretation.unwrap().text,
        answer.answer,
        "SSE answer must classify model text as generated interpretation"
    );
    assert!(answer.verified);
    assert_eq!(answer.citations.len(), 1);
    assert_eq!(model_server.chat_requests(), 2);
    assert!(model_server
        .chat_payloads()
        .iter()
        .all(|payload| payload["stream"] == false));
}

#[tokio::test]
async fn ask_stream_with_verifier_publishes_revision_without_superseded_draft() {
    let raw_draft = "The unsafe draft says beta [E1].";
    let revised = "The safe answer is alpha [E1].";
    let (body, model_server) = ask_stream_body(
        "ask-stream-verifier-revise",
        true,
        &[
            raw_draft,
            r#"{"verdict":"revise","unsupported_claims":["beta"]}"#,
            revised,
            r#"{"verdict":"pass","unsupported_claims":[]}"#,
        ],
        false,
    )
    .await;

    assert!(event_data(&body, "token").is_empty(), "SSE: {body}");
    let answers = event_data(&body, "answer");
    assert_eq!(answers.len(), 1, "SSE: {body}");
    let answer: AskResponse = serde_json::from_str(answers[0]).unwrap();
    assert!(answer.answer.starts_with(revised));
    assert!(answer.verified);
    assert_eq!(answer.citations.len(), 1);
    assert!(!body.contains(raw_draft), "SSE leaked draft: {body}");
    assert_eq!(model_server.chat_requests(), 4);
    assert!(model_server
        .chat_payloads()
        .iter()
        .all(|payload| payload["stream"] == false));
}

#[tokio::test]
async fn ask_stream_with_invalid_verifier_publishes_only_safe_error() {
    let raw_draft = "The private unsafe draft must never be published [E1].";
    let (body, model_server) = ask_stream_body(
        "ask-stream-verifier-invalid",
        true,
        &[raw_draft, "not valid verifier JSON"],
        false,
    )
    .await;

    assert!(event_data(&body, "token").is_empty(), "SSE: {body}");
    assert!(event_data(&body, "answer").is_empty(), "SSE: {body}");
    let errors = event_data(&body, "error");
    assert_eq!(errors.len(), 1, "SSE: {body}");
    let error: AskErrorEvent = serde_json::from_str(errors[0]).unwrap();
    assert_eq!(error.status, Some(500));
    assert_eq!(error.identity.kind.as_str(), "ask_error_event");
    assert_eq!(error.identity.artifact_id, "ask-stream-error");
    assert_eq!(error.identity.schema_version, WIRE_SCHEMA_VERSION);
    assert!(!error.identity.content_hash.as_str().is_empty());
    assert!(
        error.error.starts_with("verifier returned invalid JSON:"),
        "unexpected error: {}",
        error.error
    );
    assert!(!body.contains(raw_draft), "SSE leaked draft: {body}");
    assert_eq!(model_server.chat_requests(), 2);
    assert!(model_server
        .chat_payloads()
        .iter()
        .all(|payload| payload["stream"] == false));
}

#[tokio::test]
async fn ask_stream_without_verifier_preserves_token_streaming() {
    let raw_answer = "The unverified streamed answer is alpha [E1].";
    let (body, model_server) =
        ask_stream_body("ask-stream-verifier-disabled", false, &[raw_answer], false).await;

    let tokens = event_data(&body, "token");
    assert_eq!(tokens.len(), 1, "SSE: {body}");
    let token: AskTokenEvent = serde_json::from_str(tokens[0]).unwrap();
    assert_eq!(token.text, raw_answer);
    let token_wire: serde_json::Value = serde_json::from_str(tokens[0]).unwrap();
    assert_eq!(token_wire["identity"]["kind"], "ask_token_event");
    assert_eq!(token_wire["identity"]["artifact_id"], "ask-stream-token");
    assert_eq!(
        token_wire["identity"]["schema_version"],
        serde_json::to_value(WIRE_SCHEMA_VERSION).unwrap()
    );
    assert!(token_wire["identity"]["content_hash"]
        .as_str()
        .is_some_and(|hash| !hash.is_empty()));
    assert_eq!(event_data(&body, "citation").len(), 1, "SSE: {body}");
    assert!(event_data(&body, "answer").is_empty(), "SSE: {body}");
    assert_eq!(model_server.chat_requests(), 1);
    assert_eq!(model_server.chat_payloads()[0]["stream"], true);
}

#[tokio::test]
async fn default_generated_ask_stream_publishes_terminal_identities() {
    let raw_answer = "The unverified streamed answer is alpha [E1].";
    let (body, _model_server) = ask_stream_body(
        "ask-stream-generated-interpretation",
        false,
        &[raw_answer],
        false,
    )
    .await;

    let citations = event_data(&body, "citation");
    assert_eq!(citations.len(), 1, "SSE: {body}");
    let citation_wire: serde_json::Value = serde_json::from_str(citations[0]).unwrap();
    assert_eq!(citation_wire["identity"]["kind"], "ask_citation_event");
    assert_eq!(
        citation_wire["identity"]["artifact_id"],
        "ask-stream-citation"
    );
    assert_eq!(
        citation_wire["identity"]["schema_version"],
        serde_json::to_value(WIRE_SCHEMA_VERSION).unwrap()
    );
    assert!(citation_wire["identity"]["content_hash"]
        .as_str()
        .is_some_and(|hash| !hash.is_empty()));
    let citation: AskCitationEvent = serde_json::from_value(citation_wire.clone()).unwrap();
    let mut mismatched_citation = citation_wire;
    mismatched_citation["identity"]["kind"] = serde_json::json!("derived_artifact");
    serde_json::from_value::<AskCitationEvent>(mismatched_citation)
        .expect_err("stream citation identity mismatch must fail closed");
    let completed_answer = format!(
        "{raw_answer}\n\nReferences:\n[{}] {}: {}\n",
        citation.citations[0].label, citation.citations[0].kind, citation.citations[0].locator
    );

    let interpretations = event_data(&body, "generated_interpretation");
    assert_eq!(interpretations.len(), 1, "SSE: {body}");
    let interpretation: serde_json::Value = serde_json::from_str(interpretations[0]).unwrap();
    assert_eq!(interpretation["text"], completed_answer);
    assert_eq!(interpretation["identity"]["kind"], "derived_artifact");
    assert_eq!(
        interpretation["identity"]["artifact_id"],
        "live-ask-generated-interpretation"
    );
    assert_eq!(
        interpretation["identity"]["schema_version"]["major"],
        WIRE_SCHEMA_VERSION.major
    );
    assert_eq!(
        interpretation["identity"]["schema_version"]["minor"],
        WIRE_SCHEMA_VERSION.minor
    );
    assert_eq!(
        interpretation["identity"]["schema_version"]["patch"],
        WIRE_SCHEMA_VERSION.patch
    );
    assert!(interpretation["identity"]["content_hash"]
        .as_str()
        .is_some_and(|hash| !hash.is_empty()));
    assert!(interpretation.get("header").is_none());
    assert!(interpretation.get("model_fingerprint").is_none());
    assert!(interpretation.get("source_pack_hash").is_none());

    let ask_runs = event_data(&body, "ask_run");
    assert_eq!(ask_runs.len(), 1, "SSE: {body}");
    let ask_run: serde_json::Value = serde_json::from_str(ask_runs[0]).unwrap();
    assert_eq!(ask_run["answer"], completed_answer);
    assert_eq!(ask_run["answer_kind"], "generated_interpretation");
    assert_eq!(ask_run["identity"]["kind"], "ask_run");
    assert_eq!(ask_run["identity"]["artifact_id"], ask_run["task_id"]);
    assert_eq!(
        ask_run["identity"]["schema_version"],
        serde_json::to_value(WIRE_SCHEMA_VERSION).unwrap()
    );
    assert!(ask_run["identity"]["content_hash"]
        .as_str()
        .is_some_and(|hash| !hash.is_empty()));
    let decoded: AskResponse = serde_json::from_value(ask_run.clone()).unwrap();
    assert_eq!(decoded.answer, completed_answer);
    assert_eq!(
        decoded.generated_interpretation.unwrap().text,
        completed_answer
    );

    let mut mismatched = ask_run;
    mismatched["identity"]["kind"] = serde_json::json!("derived_artifact");
    serde_json::from_value::<AskResponse>(mismatched)
        .expect_err("stream ask-run identity mismatch must fail closed");
    assert!(body.trim_end().ends_with(ask_runs[0]), "SSE: {body}");
}

#[tokio::test]
async fn generated_ask_stream_persists_success_before_ask_run() {
    let raw_answer = "The unverified streamed answer is alpha [E1].";
    let (body, _model_server) = ask_stream_body(
        "ask-stream-success-before-ask-run",
        false,
        &[raw_answer],
        true,
    )
    .await;

    assert!(event_data(&body, "ask_run").is_empty(), "SSE: {body}");
    assert_eq!(event_data(&body, "error").len(), 1, "SSE: {body}");
}

#[tokio::test]
async fn generated_ask_stream_context_pack_stamps_sse_from_retrieve() {
    let raw_answer = "The unverified streamed answer is alpha [E1].";
    let (body, _model_server) =
        ask_stream_body("ask-stream-context-pack-stamp", false, &[raw_answer], false).await;

    assert!(event_data(&body, "answer").is_empty(), "SSE: {body}");
    let packs = event_data(&body, "context_pack");
    assert_eq!(packs.len(), 1, "SSE: {body}");
    let pack_json: serde_json::Value = serde_json::from_str(packs[0]).unwrap();
    assert!(pack_json.get("context").is_none());
    assert!(pack_json.get("answer").is_none());
    let pack = decode_context_pack_envelope_json(packs[0].as_bytes()).unwrap();
    assert!(!pack.selected_unit_ids.is_empty());
    assert_eq!(pack.header.profile_ref.as_deref(), Some("default"));
    assert!(pack.header.generation.is_some());
    assert!(pack.model_fingerprint.is_none());
}
