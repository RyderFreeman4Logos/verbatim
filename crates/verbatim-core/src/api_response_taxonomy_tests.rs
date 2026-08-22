use super::*;

#[test]
fn ask_response_taxonomy_never_labels_generated_or_interface_text_as_evidence() {
    let response = AskResponse {
        answer: "Legacy generated answer.".into(),
        answer_kind: AnswerKind::GeneratedInterpretation,
        text_taxonomy: ResponseTextTaxonomy::ask_response(),
        generated_interpretation: Some(GeneratedInterpretationResponse {
            text: "Generated interpretation.".into(),
        }),
        citations: vec![CitationResponse {
            label: "E1".into(),
            evidence_id: "ev-1".into(),
            kind: "original_text".into(),
            derived_from: None,
            collections: Vec::new(),
            locator: "doc.md L1".into(),
            text_preview: "Persisted source quote.".into(),
        }],
        verified: false,
        retrieval: None,
        context: None,
        collection_filter: None,
    };

    let encoded = serde_json::to_value(response).unwrap();
    let fields = encoded["text_taxonomy"]["fields"]
        .as_array()
        .expect("published text taxonomy");
    let plane_for = |field| {
        fields
            .iter()
            .find(|entry| entry["field"] == field)
            .map(|entry| entry["plane"].clone())
            .expect("field classification")
    };

    assert_eq!(
        plane_for("answer"),
        serde_json::json!("generated_interpretation")
    );
    assert_eq!(
        plane_for("citations[].label"),
        serde_json::json!("deterministic_interface_text")
    );
    assert_eq!(
        plane_for("citations[].text_preview"),
        serde_json::json!("evidence")
    );
}

#[test]
fn legacy_evidence_response_without_taxonomy_uses_source_bounded_plane() {
    let response: EvidenceResponse = serde_json::from_str(include_str!(
        "fixtures/legacy_evidence_response_without_taxonomy.json"
    ))
    .unwrap();

    assert!(!response.source_bounded);
    for field in ["text", "heading_path[]"] {
        let taxonomy = response
            .text_taxonomy
            .fields
            .iter()
            .find(|entry| entry.field == field)
            .expect("legacy field classification");
        assert_eq!(taxonomy.plane, OutputTextPlane::GeneratedInterpretation);
    }
}

#[test]
fn response_text_taxonomy_round_trips_all_four_planes() {
    let taxonomy = ResponseTextTaxonomy::ask_response();
    let encoded = serde_json::to_value(&taxonomy).unwrap();
    let decoded: ResponseTextTaxonomy = serde_json::from_value(encoded).unwrap();

    assert_eq!(decoded, taxonomy);
    assert_eq!(taxonomy.version, 1);
    for plane in [
        OutputTextPlane::Evidence,
        OutputTextPlane::Metadata,
        OutputTextPlane::DeterministicInterfaceText,
        OutputTextPlane::GeneratedInterpretation,
    ] {
        assert!(taxonomy.fields.iter().any(|field| field.plane == plane));
    }
}
