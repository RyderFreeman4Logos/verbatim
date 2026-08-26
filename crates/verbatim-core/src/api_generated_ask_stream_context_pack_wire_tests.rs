use super::{retrieve_envelope, RetrieveResponse};
use crate::wire_schemas::{decode_context_pack_envelope_json, ContextPackEnvelope};

fn sample_stream_context(evidence_id: &str) -> RetrieveResponse {
    RetrieveResponse::from_executed_ask_units(
        "What is cited?",
        "default",
        Some("7".into()),
        [evidence_id],
    )
}

fn sample_stream_pack(evidence_id: &str) -> (RetrieveResponse, ContextPackEnvelope) {
    let context = sample_stream_context(evidence_id);
    let pack = retrieve_envelope::generated_ask_stream_context_pack(Some(&context), None)
        .unwrap()
        .expect("generated ask stream must carry ContextPackEnvelope");
    (context, pack)
}

#[test]
fn generated_ask_stream_context_pack_stamps_executed_retrieve() {
    let context = sample_stream_context("ev-1");
    let pack = retrieve_envelope::generated_ask_stream_context_pack(Some(&context), None)
        .unwrap()
        .expect("generated ask stream must carry ContextPackEnvelope");
    let encoded = serde_json::to_value(&pack).unwrap();
    assert!(encoded.get("context").is_none());
    assert!(encoded.get("answer").is_none());
    let pack = decode_context_pack_envelope_json(
        serde_json::to_vec(&encoded)
            .expect("generated ask stream ContextPackEnvelope encodes")
            .as_slice(),
    )
    .unwrap();
    assert_eq!(pack.selected_unit_ids, vec!["ev-1".to_string()]);
    assert_eq!(pack.header.profile_ref.as_deref(), Some("default"));
    assert_eq!(pack.header.generation.as_deref(), Some("7"));
    assert!(pack.model_fingerprint.is_none());
}

#[test]
fn generated_ask_stream_context_pack_mismatch_is_rejected() {
    let (context, pack) = sample_stream_pack("ev-1");
    let mut encoded = serde_json::to_value(&pack).unwrap();
    encoded["selected_unit_ids"] = serde_json::json!(["ev-other"]);
    let supplied = serde_json::from_value(encoded).unwrap();
    retrieve_envelope::generated_ask_stream_context_pack(Some(&context), Some(&supplied))
        .expect_err("context pack ids must match executed retrieve units");

    let (context, pack) = sample_stream_pack("ev-1");
    let mut encoded = serde_json::to_value(&pack).unwrap();
    encoded["header"]["profile_ref"] = serde_json::json!("other");
    let supplied = serde_json::from_value(encoded).unwrap();
    retrieve_envelope::generated_ask_stream_context_pack(Some(&context), Some(&supplied))
        .expect_err("context pack profile_ref must match the executed embedding profile");

    let (context, pack) = sample_stream_pack("ev-1");
    let mut encoded = serde_json::to_value(&pack).unwrap();
    encoded["header"]["generation"] = serde_json::json!("other");
    let supplied = serde_json::from_value(encoded).unwrap();
    retrieve_envelope::generated_ask_stream_context_pack(Some(&context), Some(&supplied))
        .expect_err("context pack generation must match the executed index generation");
}

#[test]
fn generated_ask_stream_context_pack_empty_retrieve_omits() {
    let mut context = sample_stream_context("ev-1");
    context.results.clear();
    context.total_results = 0;
    context.returned_results = 0;
    let pack = retrieve_envelope::generated_ask_stream_context_pack(Some(&context), None).unwrap();
    assert!(pack.is_none());
}
