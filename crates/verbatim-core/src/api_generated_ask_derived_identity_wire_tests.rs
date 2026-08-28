use super::{AnswerKind, AskResponse, GeneratedInterpretationResponse, ResponseTextTaxonomy};
use crate::wire_schemas::WIRE_SCHEMA_VERSION;

fn sample_generated_ask() -> AskResponse {
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
        context: None,
        collection_filter: None,
    }
}

#[test]
fn generated_ask_derived_identity_stamps_answer_text() {
    let response = sample_generated_ask();
    let encoded = serde_json::to_value(&response).unwrap();
    let generated = &encoded["generated_interpretation"];

    assert_eq!(generated["text"], response.answer);
    assert_eq!(generated["identity"]["kind"], "derived_artifact");
    assert_eq!(
        generated["identity"]["artifact_id"],
        "live-ask-generated-interpretation"
    );
    assert_eq!(
        generated["identity"]["schema_version"]["major"],
        WIRE_SCHEMA_VERSION.major
    );
    assert_eq!(
        generated["identity"]["schema_version"]["minor"],
        WIRE_SCHEMA_VERSION.minor
    );
    assert_eq!(
        generated["identity"]["schema_version"]["patch"],
        WIRE_SCHEMA_VERSION.patch
    );
    assert!(generated["identity"]["content_hash"]
        .as_str()
        .is_some_and(|hash| !hash.is_empty()));
    assert!(generated.get("header").is_none());
    assert!(generated.get("model_fingerprint").is_none());
    assert!(generated.get("source_pack_hash").is_none());

    let decoded: AskResponse = serde_json::from_value(encoded).unwrap();
    assert_eq!(
        decoded.generated_interpretation,
        response.generated_interpretation
    );
}

#[test]
fn generated_ask_derived_identity_mismatch_is_rejected() {
    let mut encoded = serde_json::to_value(sample_generated_ask()).unwrap();

    encoded["generated_interpretation"]["identity"]["artifact_id"] =
        serde_json::json!("other-generated-answer");
    let error = serde_json::from_value::<AskResponse>(encoded)
        .expect_err("generated interpretation identity must fail closed");
    assert!(
        error
            .to_string()
            .contains("generated interpretation identity does not match the executed answer text"),
        "unexpected generated interpretation error: {error}"
    );
}
