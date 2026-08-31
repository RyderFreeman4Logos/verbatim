use super::{
    AnswerKind, AskRequest, AskResponse, AuditReceipt, AuditReceiptResult, ResponseTextTaxonomy,
    RetrieveControlsResponse, RetrieveRequest, RetrieveResponse, RetrieveResultResponse,
    RetrieveTimingResponse, AUDIT_RECEIPT_VERSION,
};
use crate::wire_schemas::{
    decode_context_pack_envelope_json, decode_evidence_pack_envelope_json,
    decode_query_plan_envelope_json, ContextPackEnvelope, ContextPackFields, EvidencePackEnvelope,
    QueryPlanEnvelope, QueryPlanFields, WIRE_SCHEMA_VERSION,
};

macro_rules! assert_ask_decode_error_contains {
    ($encoded:expr, $expected:expr) => {
        assert!(serde_json::from_value::<AskResponse>($encoded)
            .expect_err("malformed ask response must fail closed")
            .to_string()
            .contains($expected));
    };
}

const SELECTED_IDS_ERROR: &str = "context pack selected_unit_ids do not match context results";
const EVIDENCE_HASH_ERROR: &str = "context pack evidence_pack_hash does not match returned context";
const BLANK_RESULT_ID_ERROR: &str = "results[].evidence_id must not be blank";
const BLANK_SELECTED_ID_ERROR: &str = "selected_unit_ids must not contain empty entries";
const PROFILE_ERROR: &str =
    "context pack profile_ref does not match the executed embedding profile";
const GENERATION_ERROR: &str =
    "context pack generation does not match the executed index generation";

fn sample_query_plan() -> QueryPlanEnvelope {
    QueryPlanEnvelope::new(QueryPlanFields {
        artifact_id: "qp-live-retrieve".into(),
        query_text: "What is cited?".into(),
        steps: vec!["lexical".into()],
        generation: Some("gen-1".into()),
        profile_ref: Some("profile:default".into()),
    })
    .unwrap()
}

pub(super) fn sample_retrieve_response(query: &str, evidence_id: &str) -> RetrieveResponse {
    RetrieveResponse {
        task_id: "task-1".into(),
        query: query.into(),
        text_taxonomy: ResponseTextTaxonomy::retrieve_response(),
        source_id: None,
        collection_filter: None,
        embedding_profile_id: "default".into(),
        query_plan: None,
        evidence_pack: None,
        generation: Some("7".into()),
        limit: 12,
        page_size: 1,
        page: 1,
        total_results: 1,
        returned_results: 1,
        source_bounded: true,
        controls: RetrieveControlsResponse {
            fast: false,
            rerank_enabled: false,
            dense_top_k: 80,
            bm25_top_k: 50,
            rrf_k: 60,
            rerank_top_n: 12,
        },
        audit_receipt: AuditReceipt {
            version: AUDIT_RECEIPT_VERSION,
            embedding_profile_id: "default".into(),
            source_bounded: true,
            controls: RetrieveControlsResponse {
                fast: false,
                rerank_enabled: false,
                dense_top_k: 80,
                bm25_top_k: 50,
                rrf_k: 60,
                rerank_top_n: 12,
            },
            results: vec![AuditReceiptResult {
                evidence_id: evidence_id.into(),
                text_hash: "verified-text-hash".into(),
                source_hash: "persisted-source-hash".into(),
            }],
        },
        timings: vec![RetrieveTimingResponse {
            phase: "retrieval".into(),
            duration_ms: 7,
        }],
        results: vec![RetrieveResultResponse {
            index: 0,
            rank: 1,
            label: "E1".into(),
            evidence_id: evidence_id.into(),
            text_hash: "verified-text-hash".into(),
            source_id: "src-1".into(),
            source_hash: "persisted-source-hash".into(),
            source_path: Some("/tmp/doc.md".into()),
            collections: Vec::new(),
            chunk_id: "chunk-1".into(),
            kind: "text".into(),
            role: "original_text".into(),
            score: 0.03,
            locator: "/tmp/doc.md L1".into(),
            structured_locator: None,
            provenance: None,
            derived_from: None,
            snippet: "compact cited text".into(),
        }],
        debug: None,
    }
}

pub(super) fn sample_ask_context_response(evidence_id: &str) -> AskResponse {
    AskResponse {
        task_id: "task-1".into(),
        answer: String::new(),
        answer_kind: AnswerKind::EvidenceOnly,
        text_taxonomy: ResponseTextTaxonomy::ask_response(),
        generated_interpretation: None,
        citations: Vec::new(),
        verified: false,
        retrieval: None,
        context: Some(sample_retrieve_response("What is cited?", evidence_id)),
        collection_filter: None,
    }
}

fn context_pack_with(
    evidence_pack_hash: impl Into<String>,
    selected_unit_ids: Vec<String>,
) -> ContextPackEnvelope {
    ContextPackEnvelope::new(ContextPackFields {
        artifact_id: "live-ask-context".into(),
        evidence_pack_hash: evidence_pack_hash.into(),
        selected_unit_ids,
        model_fingerprint: None,
        generation: None,
        profile_ref: None,
    })
    .unwrap()
}

fn with_unknown_schema_version(mut envelope: serde_json::Value) -> serde_json::Value {
    let version = serde_json::json!({ "major": 99, "minor": 0, "patch": 0 });
    envelope["header"]["schema_version"] = version.clone();
    envelope["header"]["identity"]["schema_version"] = version;
    envelope
}

fn encoded_ask_context_without_pack(evidence_ids: &[&str]) -> serde_json::Value {
    let mut encoded = serde_json::to_value(sample_ask_context_response("ev-1")).unwrap();
    let template = encoded["context"]["results"][0].clone();
    let results = encoded["context"]["results"].as_array_mut().unwrap();
    results.clear();
    for index in 0..evidence_ids.len() {
        let mut result = template.clone();
        result["index"] = serde_json::json!(index);
        result["rank"] = serde_json::json!(index + 1);
        result["evidence_id"] = serde_json::json!(format!("ev-valid-{index}"));
        results.push(result);
    }
    encoded["context"]
        .as_object_mut()
        .unwrap()
        .remove("evidence_pack");
    encoded["context"]
        .as_object_mut()
        .unwrap()
        .remove("identity");
    encoded.as_object_mut().unwrap().remove("context_pack");
    let mut encoded = super::with_ask_run_identity(encoded);
    let results = encoded["context"]["results"].as_array_mut().unwrap();
    for (result, evidence_id) in results.iter_mut().zip(evidence_ids) {
        result["evidence_id"] = serde_json::json!(evidence_id);
    }
    encoded
}

fn encoded_ask_context_with_blank_context_pack_ids(evidence_ids: &[&str]) -> serde_json::Value {
    let mut encoded = serde_json::to_value(sample_ask_context_response("ev-1")).unwrap();
    let mut pack = serde_json::to_value(context_pack_with(
        context_pack_hash(&encoded),
        vec!["ev-1".into()],
    ))
    .unwrap();
    pack["selected_unit_ids"] = serde_json::json!(evidence_ids);
    encoded["context_pack"] = pack;
    super::with_ask_run_identity(encoded)
}

fn context_pack_hash(encoded: &serde_json::Value) -> String {
    encoded["context"]["evidence_pack"]["header"]["identity"]["content_hash"]
        .as_str()
        .unwrap()
        .into()
}

#[test]
fn live_retrieve_request_valid_query_plan_envelope_round_trips() {
    let plan = live_question_plan("What is cited?");
    let request: RetrieveRequest = serde_json::from_value(serde_json::json!({
        "question": plan.query_text,
        "query_plan": plan,
    }))
    .unwrap();
    assert_eq!(request.question, plan.query_text);

    let encoded = serde_json::to_value(&request).unwrap();
    let back = decode_query_plan_envelope_json(
        encoded
            .get("query_plan")
            .and_then(serde_json::Value::as_object)
            .map(|value| serde_json::to_vec(value).unwrap())
            .expect("live retrieve request must carry QueryPlanEnvelope")
            .as_slice(),
    )
    .unwrap();
    assert_eq!(back.header.schema_version, WIRE_SCHEMA_VERSION);
    assert_eq!(back.query_text, plan.query_text);
    back.validate().unwrap();
}
#[test]
fn live_retrieve_request_unknown_schema_version_is_rejected() {
    let plan = sample_query_plan();
    let question = plan.query_text.clone();
    let plan = with_unknown_schema_version(serde_json::to_value(plan).unwrap());
    serde_json::from_value::<RetrieveRequest>(serde_json::json!({
        "question": question,
        "query_plan": plan,
    }))
    .expect_err("unknown query plan schema must fail closed");
}
#[test]
fn live_retrieve_request_incomplete_identity_is_rejected() {
    let mut plan = serde_json::to_value(sample_query_plan()).unwrap();
    plan["header"]["identity"]
        .as_object_mut()
        .unwrap()
        .remove("content_hash");
    serde_json::from_value::<RetrieveRequest>(serde_json::json!({
        "question": "What is cited?",
        "query_plan": plan,
    }))
    .expect_err("incomplete query plan identity must fail closed");
}
#[test]
fn live_retrieve_request_legacy_question_only_projects_through_query_plan_envelope() {
    let request: RetrieveRequest =
        serde_json::from_value(serde_json::json!({"question": "What is cited?"})).unwrap();
    assert_eq!(request.question, "What is cited?");

    let encoded = serde_json::to_value(&request).unwrap();
    let plan = decode_query_plan_envelope_json(
        encoded
            .get("query_plan")
            .map(|value| serde_json::to_vec(value).unwrap())
            .expect("legacy question-only retrieve must project through QueryPlanEnvelope")
            .as_slice(),
    )
    .unwrap();
    assert_eq!(plan.query_text, "What is cited?");
    plan.validate().unwrap();
}
#[test]
fn live_retrieve_response_valid_evidence_pack_envelope_round_trips() {
    let response = sample_retrieve_response("What is cited?", "ev-1");
    let encoded = serde_json::to_value(&response).unwrap();
    let pack = decode_evidence_pack_envelope_json(
        encoded
            .get("evidence_pack")
            .map(|value| serde_json::to_vec(value).unwrap())
            .expect("live retrieve response must carry EvidencePackEnvelope")
            .as_slice(),
    )
    .unwrap();
    assert_eq!(pack.header.schema_version, WIRE_SCHEMA_VERSION);
    assert_eq!(pack.evidence_unit_ids, vec!["ev-1".to_string()]);
    pack.validate().unwrap();

    let back: RetrieveResponse = serde_json::from_value(encoded).unwrap();
    assert_eq!(back.results[0].evidence_id, "ev-1");
}
#[test]
fn post_retrieve_response_stamps_retrieval_run_identity() {
    let response = sample_retrieve_response("What is cited?", "ev-1");
    let encoded = serde_json::to_value(&response).unwrap();
    let identity = encoded
        .get("identity")
        .expect("normal POST retrieve response must carry retrieval-run identity");

    assert_eq!(identity["kind"], "retrieval_run");
    assert_eq!(
        identity["schema_version"],
        serde_json::json!({"major": 1, "minor": 0, "patch": 0})
    );
    assert_eq!(identity["artifact_id"], response.task_id);
    assert!(identity["content_hash"]
        .as_str()
        .is_some_and(|hash| !hash.is_empty()));
    assert!(encoded["text_taxonomy"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .any(|field| field["field"] == "identity.content_hash" && field["plane"] == "metadata"));

    let back: RetrieveResponse = serde_json::from_value(encoded).unwrap();
    assert_eq!(back.results, response.results);
}
#[test]
fn post_retrieve_response_retrieval_run_identity_mismatch_fails_closed() {
    let mut encoded =
        serde_json::to_value(sample_retrieve_response("What is cited?", "ev-1")).unwrap();
    encoded["identity"] = serde_json::json!({
        "kind": "query_plan",
        "schema_version": {"major": 1, "minor": 0, "patch": 0},
        "artifact_id": "task-1",
        "content_hash": "mismatched-retrieval-run-body"
    });

    serde_json::from_value::<RetrieveResponse>(encoded)
        .expect_err("retrieval-run identity must match the executed response body");
}
#[test]
fn post_ask_response_stamps_ask_run_identity() {
    let response = sample_ask_context_response("ev-1");
    let encoded = serde_json::to_value(&response).unwrap();
    let identity = encoded
        .get("identity")
        .expect("normal POST ask response must carry ask-run identity");

    assert_eq!(encoded["task_id"], "task-1");
    assert_eq!(identity["kind"], "ask_run");
    assert_eq!(
        identity["schema_version"],
        serde_json::to_value(WIRE_SCHEMA_VERSION).unwrap()
    );
    assert_eq!(identity["artifact_id"], "task-1");
    assert!(identity["content_hash"]
        .as_str()
        .is_some_and(|hash| !hash.is_empty()));
    assert!(encoded["text_taxonomy"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .any(|field| field["field"] == "identity.content_hash" && field["plane"] == "metadata"));

    let back: AskResponse = serde_json::from_value(encoded).unwrap();
    assert_eq!(back.task_id, response.task_id);
    assert_eq!(
        back.context.unwrap().results,
        response.context.unwrap().results,
    );
}
#[test]
fn post_ask_response_ask_run_identity_mismatch_fails_closed() {
    let mut encoded = serde_json::to_value(sample_ask_context_response("ev-1")).unwrap();
    encoded["task_id"] = serde_json::json!("other-task");
    encoded["identity"] = serde_json::json!({
        "kind": "query_plan",
        "schema_version": {"major": 1, "minor": 0, "patch": 0},
        "artifact_id": "task-1",
        "content_hash": "mismatched-ask-run-body"
    });

    serde_json::from_value::<AskResponse>(encoded)
        .expect_err("ask-run identity must match the executed response body");
}
#[test]
fn live_ask_context_pack_valid_envelope_round_trips() {
    let response = sample_ask_context_response("ev-1");
    let encoded = serde_json::to_value(&response).unwrap();
    let pack = decode_context_pack_envelope_json(
        encoded
            .get("context_pack")
            .map(|value| serde_json::to_vec(value).unwrap())
            .expect("live ask context response must carry ContextPackEnvelope")
            .as_slice(),
    )
    .unwrap();

    assert_eq!(pack.header.schema_version, WIRE_SCHEMA_VERSION);
    assert_eq!(pack.evidence_pack_hash, context_pack_hash(&encoded));
    assert_eq!(pack.selected_unit_ids, vec!["ev-1".to_string()]);
    pack.validate().unwrap();
    assert!(encoded["text_taxonomy"]["fields"]
        .as_array()
        .unwrap()
        .iter()
        .any(|field| field["field"] == "context_pack.header.identity.content_hash"));

    let back: AskResponse = serde_json::from_value(encoded).unwrap();
    assert_eq!(back.context.unwrap().results[0].evidence_id, "ev-1");
}
#[test]
fn live_ask_context_pack_mismatched_payload_vs_envelope_is_rejected() {
    let mut encoded = serde_json::to_value(sample_ask_context_response("ev-1")).unwrap();
    encoded["context_pack"] = serde_json::to_value(context_pack_with(
        context_pack_hash(&encoded),
        vec!["ev-other".into()],
    ))
    .unwrap();
    let encoded = super::with_ask_run_identity(encoded);

    assert_ask_decode_error_contains!(encoded, SELECTED_IDS_ERROR);
}
#[test]
fn live_ask_context_pack_noncanonical_evidence_hash_is_rejected() {
    let mut encoded = serde_json::to_value(sample_ask_context_response("ev-1")).unwrap();
    encoded["context_pack"] = serde_json::to_value(context_pack_with(
        "other-evidence-pack",
        vec!["ev-1".into()],
    ))
    .unwrap();
    let encoded = super::with_ask_run_identity(encoded);

    assert_ask_decode_error_contains!(encoded, EVIDENCE_HASH_ERROR);
}
#[test]
fn live_ask_context_pack_incomplete_or_unknown_identity_is_rejected() {
    let mut incomplete = serde_json::to_value(sample_ask_context_response("ev-1")).unwrap();
    let mut pack = serde_json::to_value(context_pack_with(
        context_pack_hash(&incomplete),
        vec!["ev-1".into()],
    ))
    .unwrap();
    pack["header"]["identity"]
        .as_object_mut()
        .unwrap()
        .remove("content_hash");
    incomplete["context_pack"] = pack;
    assert_ask_decode_error_contains!(incomplete, "missing field `content_hash`");

    let mut unknown = serde_json::to_value(sample_ask_context_response("ev-1")).unwrap();
    let pack = with_unknown_schema_version(
        serde_json::to_value(context_pack_with(
            context_pack_hash(&unknown),
            vec!["ev-1".into()],
        ))
        .unwrap(),
    );
    unknown["context_pack"] = pack;
    assert_ask_decode_error_contains!(unknown, "unsupported wire schema version");
}
#[test]
fn live_retrieve_response_unknown_schema_version_is_rejected() {
    let mut encoded =
        serde_json::to_value(sample_retrieve_response("What is cited?", "ev-1")).unwrap();
    let plan = sample_query_plan();
    let pack = EvidencePackEnvelope::new(crate::wire_schemas::EvidencePackFields {
        artifact_id: "ep-live-retrieve".into(),
        evidence_unit_ids: vec!["ev-1".into()],
        query_plan_hash: plan.header.identity.content_hash.as_str().into(),
        generation: None,
        profile_ref: None,
    })
    .unwrap();
    encoded["evidence_pack"] = with_unknown_schema_version(serde_json::to_value(pack).unwrap());
    serde_json::from_value::<RetrieveResponse>(encoded)
        .expect_err("unknown evidence pack schema must fail closed");
}
#[test]
fn live_retrieve_response_incomplete_identity_is_rejected() {
    let mut encoded =
        serde_json::to_value(sample_retrieve_response("What is cited?", "ev-1")).unwrap();
    let plan = sample_query_plan();
    let pack = EvidencePackEnvelope::new(crate::wire_schemas::EvidencePackFields {
        artifact_id: "ep-live-retrieve".into(),
        evidence_unit_ids: vec!["ev-1".into()],
        query_plan_hash: plan.header.identity.content_hash.as_str().into(),
        generation: None,
        profile_ref: None,
    })
    .unwrap();
    let mut pack_json = serde_json::to_value(pack).unwrap();
    pack_json["header"]["identity"]
        .as_object_mut()
        .unwrap()
        .remove("content_hash");
    encoded["evidence_pack"] = pack_json;
    serde_json::from_value::<RetrieveResponse>(encoded)
        .expect_err("incomplete evidence pack identity must fail closed");
}
#[test]
fn live_retrieve_request_mismatched_question_vs_envelope_is_rejected() {
    let plan = sample_query_plan();
    serde_json::from_value::<RetrieveRequest>(serde_json::json!({
        "question": "A different question",
        "query_plan": plan,
    }))
    .expect_err("request envelope must match the executed question");
}
#[test]
fn live_retrieve_request_noncanonical_plan_hash_is_rejected() {
    let plan = sample_query_plan();
    assert_eq!(plan.query_text, "What is cited?");
    assert!(!plan.steps.is_empty());
    serde_json::from_value::<RetrieveRequest>(serde_json::json!({
        "question": plan.query_text,
        "query_plan": plan,
    }))
    .expect_err("request plan hash must match the effective question plan");
}

fn live_question_plan(question: &str) -> QueryPlanEnvelope {
    QueryPlanEnvelope::new(QueryPlanFields {
        artifact_id: "live-retrieve".into(),
        query_text: question.into(),
        steps: Vec::new(),
        generation: None,
        profile_ref: None,
    })
    .unwrap()
}
#[test]
fn live_retrieve_response_mismatched_evidence_ids_are_rejected() {
    let mut encoded =
        serde_json::to_value(sample_retrieve_response("What is cited?", "ev-1")).unwrap();
    let plan = live_question_plan("What is cited?");
    let pack = EvidencePackEnvelope::new(crate::wire_schemas::EvidencePackFields {
        artifact_id: "ep-live-retrieve".into(),
        evidence_unit_ids: vec!["ev-other".into()],
        query_plan_hash: plan.header.identity.content_hash.as_str().into(),
        generation: None,
        profile_ref: None,
    })
    .unwrap();
    pack.validate().unwrap();
    encoded["evidence_pack"] = serde_json::to_value(pack).unwrap();
    serde_json::from_value::<RetrieveResponse>(encoded)
        .expect_err("evidence pack ids must match results[].evidence_id");
}
#[test]
fn live_retrieve_response_mismatched_query_plan_hash_is_rejected() {
    let mut encoded =
        serde_json::to_value(sample_retrieve_response("What is cited?", "ev-1")).unwrap();
    let other = live_question_plan("A different question");
    let pack = EvidencePackEnvelope::new(crate::wire_schemas::EvidencePackFields {
        artifact_id: "ep-live-retrieve".into(),
        evidence_unit_ids: vec!["ev-1".into()],
        query_plan_hash: other.header.identity.content_hash.as_str().into(),
        generation: None,
        profile_ref: None,
    })
    .unwrap();
    pack.validate().unwrap();
    encoded["evidence_pack"] = serde_json::to_value(pack).unwrap();
    serde_json::from_value::<RetrieveResponse>(encoded)
        .expect_err("evidence pack query_plan_hash must match the adjacent query");
}

fn mixed_blank_retrieve_response() -> RetrieveResponse {
    let mut response = sample_retrieve_response("What is cited?", "ev-1");
    let mut blank = response.results[0].clone();
    blank.index = 1;
    blank.rank = 2;
    blank.label = "E2".into();
    blank.evidence_id = " ".into();
    response.results.push(blank);
    response.total_results = 2;
    response.returned_results = 2;
    response
}

fn encoded_retrieve_without_pack(evidence_ids: &[&str]) -> serde_json::Value {
    let mut encoded =
        serde_json::to_value(sample_retrieve_response("What is cited?", "ev-1")).unwrap();
    let template = encoded["results"][0].clone();
    let results = encoded["results"].as_array_mut().unwrap();
    results.clear();
    for (index, evidence_id) in evidence_ids.iter().enumerate() {
        let mut result = template.clone();
        result["index"] = serde_json::json!(index);
        result["rank"] = serde_json::json!(index + 1);
        result["evidence_id"] = serde_json::json!(evidence_id);
        results.push(result);
    }
    encoded.as_object_mut().unwrap().remove("evidence_pack");
    encoded
}
#[test]
fn live_retrieve_response_mixed_blank_evidence_id_serialize_is_rejected() {
    serde_json::to_value(mixed_blank_retrieve_response())
        .expect_err("mixed blank results[].evidence_id must fail closed on serialize");
}
#[test]
fn live_retrieve_response_mixed_blank_evidence_id_deserialize_is_rejected() {
    serde_json::from_value::<RetrieveResponse>(encoded_retrieve_without_pack(&["ev-1", " "]))
        .expect_err("mixed blank results[].evidence_id must fail closed on deserialize");
}
#[test]
fn live_retrieve_response_all_blank_evidence_id_serialize_is_rejected() {
    serde_json::to_value(sample_retrieve_response("What is cited?", " "))
        .expect_err("all-blank results[].evidence_id must fail closed on serialize");
}
#[test]
fn live_retrieve_response_all_blank_evidence_id_deserialize_is_rejected() {
    serde_json::from_value::<RetrieveResponse>(encoded_retrieve_without_pack(&[" "]))
        .expect_err("all-blank results[].evidence_id must fail closed on deserialize");
}
#[test]
fn live_ask_context_pack_mixed_blank_ids_serialize_and_deserialize_are_rejected() {
    let mut response = sample_ask_context_response("ev-1");
    let context = response.context.as_mut().unwrap();
    let mut blank = context.results[0].clone();
    blank.index = 1;
    blank.rank = 2;
    blank.label = "E2".into();
    blank.evidence_id = " ".into();
    context.results.push(blank);
    context.total_results = 2;
    context.returned_results = 2;
    serde_json::to_value(response)
        .expect_err("mixed blank context results[].evidence_id must fail closed on serialize");

    assert_ask_decode_error_contains!(
        encoded_ask_context_without_pack(&["ev-1", " "]),
        BLANK_RESULT_ID_ERROR
    );
    assert_ask_decode_error_contains!(
        encoded_ask_context_with_blank_context_pack_ids(&["ev-1", " "]),
        BLANK_SELECTED_ID_ERROR
    );
}
#[test]
fn live_ask_context_pack_all_blank_ids_serialize_and_deserialize_are_rejected() {
    serde_json::to_value(sample_ask_context_response(" "))
        .expect_err("all-blank context results[].evidence_id must fail closed on serialize");

    assert_ask_decode_error_contains!(
        encoded_ask_context_without_pack(&[" "]),
        BLANK_RESULT_ID_ERROR
    );
    assert_ask_decode_error_contains!(
        encoded_ask_context_with_blank_context_pack_ids(&[" "]),
        BLANK_SELECTED_ID_ERROR
    );
}
#[test]
fn live_envelope_profile_ref_retrieve_request_stamps_executed_profile() {
    let request: RetrieveRequest = serde_json::from_value(serde_json::json!({
        "question": "What is cited?",
        "embedding_profile_id": "default",
    }))
    .unwrap();
    let encoded = serde_json::to_value(&request).unwrap();
    assert_eq!(encoded["query_plan"]["header"]["profile_ref"], "default");
}
#[test]
fn live_envelope_profile_ref_retrieve_response_stamps_executed_profile() {
    let encoded = serde_json::to_value(sample_retrieve_response("What is cited?", "ev-1")).unwrap();
    assert_eq!(encoded["evidence_pack"]["header"]["profile_ref"], "default");
}
#[test]
fn live_envelope_profile_ref_ask_context_stamps_executed_profile() {
    let encoded = serde_json::to_value(sample_ask_context_response("ev-1")).unwrap();
    assert_eq!(encoded["context_pack"]["header"]["profile_ref"], "default");
}
#[test]
fn live_envelope_profile_ref_retrieve_request_mismatch_is_rejected() {
    let mut plan = serde_json::to_value(live_question_plan("What is cited?")).unwrap();
    plan["header"]["profile_ref"] = serde_json::json!("other");
    serde_json::from_value::<RetrieveRequest>(serde_json::json!({
        "question": "What is cited?",
        "embedding_profile_id": "default",
        "query_plan": plan,
    }))
    .expect_err("query plan profile_ref must match the executed embedding profile");
}
#[test]
fn live_envelope_profile_ref_retrieve_response_mismatch_is_rejected() {
    let mut encoded =
        serde_json::to_value(sample_retrieve_response("What is cited?", "ev-1")).unwrap();
    encoded["evidence_pack"]["header"]["profile_ref"] = serde_json::json!("other");
    serde_json::from_value::<RetrieveResponse>(encoded)
        .expect_err("evidence pack profile_ref must match the executed embedding profile");
}
#[test]
fn live_envelope_profile_ref_ask_context_mismatch_is_rejected() {
    let mut encoded = serde_json::to_value(sample_ask_context_response("ev-1")).unwrap();
    encoded["context_pack"]["header"]["profile_ref"] = serde_json::json!("other");
    let encoded = super::with_ask_run_identity(encoded);
    assert_ask_decode_error_contains!(encoded, PROFILE_ERROR);
}
#[test]
fn live_envelope_generation_retrieve_response_stamps_executed_generation() {
    let encoded = serde_json::to_value(sample_retrieve_response("What is cited?", "ev-1")).unwrap();
    assert_eq!(encoded["evidence_pack"]["header"]["generation"], "7");
}
#[test]
fn live_envelope_generation_ask_context_stamps_executed_generation() {
    let encoded = serde_json::to_value(sample_ask_context_response("ev-1")).unwrap();
    assert_eq!(encoded["context_pack"]["header"]["generation"], "7");
}
#[test]
fn live_envelope_generation_retrieve_response_mismatch_is_rejected() {
    let mut encoded =
        serde_json::to_value(sample_retrieve_response("What is cited?", "ev-1")).unwrap();
    encoded["evidence_pack"]["header"]["generation"] = serde_json::json!("other");
    serde_json::from_value::<RetrieveResponse>(encoded)
        .expect_err("evidence pack generation must match the executed index generation");
}
#[test]
fn live_envelope_generation_ask_context_mismatch_is_rejected() {
    let mut encoded = serde_json::to_value(sample_ask_context_response("ev-1")).unwrap();
    encoded["context_pack"]["header"]["generation"] = serde_json::json!("other");
    let encoded = super::with_ask_run_identity(encoded);
    assert_ask_decode_error_contains!(encoded, GENERATION_ERROR);
}
#[test]
fn live_ask_request_valid_query_plan_envelope_round_trips() {
    let plan = live_question_plan_with_profile("What is cited?", "default");
    let request: AskRequest = serde_json::from_value(serde_json::json!({
        "question": plan.query_text,
        "embedding_profile_id": "default",
        "query_plan": plan,
    }))
    .unwrap();
    assert_eq!(request.question, plan.query_text);
    assert_eq!(request.embedding_profile_id.as_deref(), Some("default"));

    let encoded = serde_json::to_value(&request).unwrap();
    let back = decode_query_plan_envelope_json(
        encoded
            .get("query_plan")
            .and_then(serde_json::Value::as_object)
            .map(|value| serde_json::to_vec(value).unwrap())
            .expect("live ask request must carry QueryPlanEnvelope")
            .as_slice(),
    )
    .unwrap();
    assert_eq!(back.header.schema_version, WIRE_SCHEMA_VERSION);
    assert_eq!(back.query_text, plan.query_text);
    assert_eq!(back.header.profile_ref.as_deref(), Some("default"));
    assert!(back.header.generation.is_none());
    back.validate().unwrap();
}
#[test]
fn live_ask_request_legacy_question_only_projects_through_query_plan_envelope() {
    let request: AskRequest =
        serde_json::from_value(serde_json::json!({"question": "What is cited?"})).unwrap();
    assert_eq!(request.question, "What is cited?");

    let encoded = serde_json::to_value(&request).unwrap();
    let plan = decode_query_plan_envelope_json(
        encoded
            .get("query_plan")
            .map(|value| serde_json::to_vec(value).unwrap())
            .expect("legacy question-only ask must project through QueryPlanEnvelope")
            .as_slice(),
    )
    .unwrap();
    assert_eq!(plan.query_text, "What is cited?");
    assert!(plan.header.generation.is_none());
    plan.validate().unwrap();
}
#[test]
fn live_ask_request_mismatched_question_vs_envelope_is_rejected() {
    let plan = sample_query_plan();
    serde_json::from_value::<AskRequest>(serde_json::json!({
        "question": "A different question",
        "query_plan": plan,
    }))
    .expect_err("request envelope must match the executed question");
}
#[test]
fn live_ask_request_mismatched_profile_vs_envelope_is_rejected() {
    let mut plan = serde_json::to_value(live_question_plan("What is cited?")).unwrap();
    plan["header"]["profile_ref"] = serde_json::json!("other");
    serde_json::from_value::<AskRequest>(serde_json::json!({
        "question": "What is cited?",
        "embedding_profile_id": "default",
        "query_plan": plan,
    }))
    .expect_err("query plan profile_ref must match the executed embedding profile");
}

fn live_question_plan_with_profile(question: &str, profile: &str) -> QueryPlanEnvelope {
    QueryPlanEnvelope::new(QueryPlanFields {
        artifact_id: "live-retrieve".into(),
        query_text: question.into(),
        steps: Vec::new(),
        generation: None,
        profile_ref: Some(profile.into()),
    })
    .unwrap()
}
