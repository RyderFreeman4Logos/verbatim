use super::{
    AuditReceipt, AuditReceiptResult, ResponseTextTaxonomy, RetrieveControlsResponse,
    RetrieveRequest, RetrieveResponse, RetrieveResultResponse, RetrieveTimingResponse,
    AUDIT_RECEIPT_VERSION,
};
use crate::wire_schemas::{
    decode_evidence_pack_envelope_json, decode_query_plan_envelope_json, EvidencePackEnvelope,
    QueryPlanEnvelope, QueryPlanFields, WireSchemaVersion, WIRE_SCHEMA_VERSION,
};

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

fn sample_retrieve_response(query: &str, evidence_id: &str) -> RetrieveResponse {
    RetrieveResponse {
        task_id: "task-1".into(),
        query: query.into(),
        text_taxonomy: ResponseTextTaxonomy::retrieve_response(),
        source_id: None,
        collection_filter: None,
        embedding_profile_id: "default".into(),
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
    let mut plan = sample_query_plan();
    plan.header.schema_version = WireSchemaVersion::new(99, 0, 0);
    plan.header.identity.schema_version = WireSchemaVersion::new(99, 0, 0);
    serde_json::from_value::<RetrieveRequest>(serde_json::json!({
        "question": plan.query_text,
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
fn live_retrieve_response_unknown_schema_version_is_rejected() {
    let mut encoded =
        serde_json::to_value(sample_retrieve_response("What is cited?", "ev-1")).unwrap();
    let plan = sample_query_plan();
    let mut pack = EvidencePackEnvelope::new(crate::wire_schemas::EvidencePackFields {
        artifact_id: "ep-live-retrieve".into(),
        evidence_unit_ids: vec!["ev-1".into()],
        query_plan_hash: plan.header.identity.content_hash.as_str().into(),
        generation: None,
        profile_ref: None,
    })
    .unwrap();
    pack.header.schema_version = WireSchemaVersion::new(2, 0, 0);
    pack.header.identity.schema_version = WireSchemaVersion::new(2, 0, 0);
    encoded["evidence_pack"] = serde_json::to_value(pack).unwrap();
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
