use super::{
    AnswerKind, AskResponse, GeneratedInterpretationResponse, ResponseTextTaxonomy,
    RetrieveResponse,
};
use crate::wire_schemas::decode_context_pack_envelope_json;

fn sample_generated_ask(evidence_id: &str) -> AskResponse {
    let mut context = RetrieveResponse::from_executed_ask_units(
        "What is cited?",
        "default",
        Some("7".into()),
        [evidence_id],
    );
    context.task_id = "generated-ask-retrieve".into();
    AskResponse {
        task_id: "generated-ask-run".into(),
        answer: "Generated interpretation.".into(),
        answer_kind: AnswerKind::GeneratedInterpretation,
        text_taxonomy: ResponseTextTaxonomy::ask_response(),
        generated_interpretation: Some(GeneratedInterpretationResponse {
            text: "Generated interpretation.".into(),
        }),
        citations: Vec::new(),
        verified: false,
        retrieval: None,
        context: Some(context),
        collection_filter: None,
    }
}

#[test]
fn live_generated_ask_context_pack_stamps_executed_retrieve() {
    let encoded = serde_json::to_value(sample_generated_ask("ev-1")).unwrap();
    assert!(encoded.get("context").is_none());
    let pack = decode_context_pack_envelope_json(
        encoded
            .get("context_pack")
            .map(|value| serde_json::to_vec(value).unwrap())
            .expect("generated ask must carry ContextPackEnvelope")
            .as_slice(),
    )
    .unwrap();
    assert_eq!(pack.selected_unit_ids, vec!["ev-1".to_string()]);
    assert_eq!(pack.header.profile_ref.as_deref(), Some("default"));
    assert_eq!(pack.header.generation.as_deref(), Some("7"));
    assert!(pack.model_fingerprint.is_none());
}

#[test]
fn live_generated_ask_context_pack_mismatch_is_rejected() {
    let response = sample_generated_ask("ev-1");
    let mut encoded = serde_json::to_value(&response).unwrap();
    assert!(encoded.get("context").is_none());

    encoded["context_pack"]["selected_unit_ids"] = serde_json::json!(["ev-other"]);
    encoded["context"] = serde_json::to_value(response.context.clone()).unwrap();
    serde_json::from_value::<AskResponse>(encoded)
        .expect_err("context pack ids must match executed retrieve units");

    let response = sample_generated_ask("ev-1");
    let mut encoded = serde_json::to_value(&response).unwrap();
    encoded["context_pack"]["header"]["profile_ref"] = serde_json::json!("other");
    encoded["context"] = serde_json::to_value(response.context.clone()).unwrap();
    serde_json::from_value::<AskResponse>(encoded)
        .expect_err("context pack profile_ref must match the executed embedding profile");

    let response = sample_generated_ask("ev-1");
    let mut encoded = serde_json::to_value(&response).unwrap();
    encoded["context_pack"]["header"]["generation"] = serde_json::json!("other");
    encoded["context"] = serde_json::to_value(response.context.clone()).unwrap();
    serde_json::from_value::<AskResponse>(encoded)
        .expect_err("context pack generation must match the executed index generation");
}

#[test]
fn live_generated_ask_empty_retrieve_omits_context_pack() {
    let mut response = sample_generated_ask("ev-1");
    let context = response.context.as_mut().unwrap();
    context.results.clear();
    context.total_results = 0;
    context.returned_results = 0;
    let encoded = serde_json::to_value(response).unwrap();
    assert!(encoded.get("context").is_none());
    assert!(encoded.get("context_pack").is_none());
}
