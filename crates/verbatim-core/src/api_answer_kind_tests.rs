#[test]
fn ask_response_serializes_generated_interpretation_separately_from_evidence() {
    let response = AskResponse {
        answer: "Legacy generated answer.".into(),
        answer_kind: AnswerKind::GeneratedInterpretation,
        generated_interpretation: Some(GeneratedInterpretationResponse {
            text: "Generated interpretation.".into(),
        }),
        citations: Vec::new(),
        verified: false,
        retrieval: None,
        context: None,
        collection_filter: None,
    };

    let encoded = serde_json::to_value(response).unwrap();

    assert_eq!(encoded["answer"], "Legacy generated answer.");
    assert_eq!(
        encoded["generated_interpretation"],
        serde_json::json!({"text": "Generated interpretation."})
    );
    assert_eq!(
        encoded["answer_kind"],
        serde_json::json!("generated_interpretation")
    );
    assert!(encoded.get("source_bounded").is_none());
    assert!(encoded.get("context").is_none());
}

#[test]
fn ask_response_serializes_context_only_as_evidence_only() {
    let response = AskResponse {
        answer: String::new(),
        answer_kind: AnswerKind::EvidenceOnly,
        generated_interpretation: None,
        citations: Vec::new(),
        verified: false,
        retrieval: None,
        context: None,
        collection_filter: None,
    };

    let encoded = serde_json::to_value(response).unwrap();

    assert_eq!(encoded["answer_kind"], serde_json::json!("evidence_only"));
}
