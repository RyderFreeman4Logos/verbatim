fn tampered_taxonomy(mut value: serde_json::Value) -> serde_json::Value {
    value["text_taxonomy"] = serde_json::json!({
        "version": 1,
        "fields": [
            {"field": "answer", "plane": "evidence"},
            {"field": "stale", "plane": "metadata"},
            {"field": "answer", "plane": "metadata"}
        ]
    });
    value
}

fn assert_taxonomy_recomputed<T: serde::Serialize>(
    name: &str,
    value: serde_json::Value,
    decode: impl FnOnce(serde_json::Value) -> T,
    taxonomy: impl FnOnce(&T) -> ResponseTextTaxonomy,
) {
    let expected = ResponseTextTaxonomy::from_serialized_value(&value);
    let response = decode(value);
    assert_eq!(taxonomy(&response), expected, "{name} taxonomy");
    assert_taxonomy_paths_resolve(name, &serde_json::to_value(response).unwrap());
}

fn assert_header_generation_is_metadata(name: &str, value: &serde_json::Value, fields: &[&str]) {
    let taxonomy = ResponseTextTaxonomy::from_serialized_value(value);
    for field in fields {
        let entry = taxonomy
            .fields
            .iter()
            .find(|entry| entry.field == *field)
            .unwrap_or_else(|| panic!("{name} missing {field}"));
        assert_eq!(
            entry.plane,
            OutputTextPlane::Metadata,
            "{name} {field} must be metadata"
        );
    }
}

#[test]
fn supplied_taxonomy_is_recomputed_for_all_response_shapes() {
    let mut ask = tampered_taxonomy(serde_json::from_str(include_str!(
        "fixtures/legacy_ask_response_without_taxonomy.json"
    ))
    .unwrap());
    ask["context"] = tampered_taxonomy(serde_json::from_str(include_str!(
        "fixtures/legacy_retrieve_caption_without_taxonomy.json"
    ))
    .unwrap());
    ask["context"]["generation"] = serde_json::json!("7");
    assert_header_generation_is_metadata(
        "ask with nested retrieve context",
        &ask,
        &[
            "context.evidence_pack.header.generation",
            "context_pack.header.generation",
        ],
    );
    assert_taxonomy_recomputed(
        "ask with nested retrieve context",
        ask,
        |value| serde_json::from_value::<AskResponse>(value).unwrap(),
        |response| response.text_taxonomy.clone(),
    );

    let mut retrieve = tampered_taxonomy(serde_json::from_str(include_str!(
        "fixtures/legacy_retrieve_caption_without_taxonomy.json"
    ))
    .unwrap());
    retrieve["generation"] = serde_json::json!("7");
    assert_header_generation_is_metadata(
        "retrieve",
        &retrieve,
        &["evidence_pack.header.generation"],
    );
    assert_taxonomy_recomputed(
        "retrieve",
        retrieve,
        |value| serde_json::from_value::<RetrieveResponse>(value).unwrap(),
        |response| response.text_taxonomy.clone(),
    );

    assert_taxonomy_recomputed(
        "evidence",
        tampered_taxonomy(serde_json::from_str(include_str!(
            "fixtures/legacy_evidence_response_without_taxonomy.json"
        ))
        .unwrap()),
        |value| serde_json::from_value::<EvidenceResponse>(value).unwrap(),
        |response| response.text_taxonomy.clone(),
    );
}

#[test]
fn evidence_response_requires_source_kind_and_boundary() {
    let cases = [
        ("text", true, OutputTextPlane::Evidence),
        ("text", false, OutputTextPlane::GeneratedInterpretation),
        ("ocr", true, OutputTextPlane::GeneratedInterpretation),
        ("generated", true, OutputTextPlane::GeneratedInterpretation),
        ("image", true, OutputTextPlane::Metadata),
        ("unknown", true, OutputTextPlane::GeneratedInterpretation),
    ];

    for (kind, source_bounded, expected_plane) in cases {
        let mut value: serde_json::Value = serde_json::from_str(include_str!(
            "fixtures/legacy_evidence_response_without_taxonomy.json"
        ))
        .unwrap();
        value["kind"] = serde_json::json!(kind);
        value["source_bounded"] = serde_json::json!(source_bounded);
        value["heading_path"] = serde_json::json!(["Heading"]);
        let response: EvidenceResponse = serde_json::from_value(value).unwrap();

        for field in ["text", "heading_path[]"] {
            assert_eq!(
                response
                    .text_taxonomy
                    .fields
                    .iter()
                    .find(|entry| entry.field == field)
                    .unwrap()
                    .plane,
                expected_plane,
                "kind={kind}, source_bounded={source_bounded}, field={field}"
            );
        }
    }
}

#[test]
fn provenance_tuples_fail_closed_and_preserve_metadata_classification() {
    let citation_cases = [
        ("original_text", OutputTextPlane::Evidence),
        ("text", OutputTextPlane::Evidence),
        ("image", OutputTextPlane::Metadata),
        ("image_artifact", OutputTextPlane::Metadata),
        ("ocr_text", OutputTextPlane::GeneratedInterpretation),
        ("generated", OutputTextPlane::GeneratedInterpretation),
        ("unknown", OutputTextPlane::GeneratedInterpretation),
    ];
    for (kind, expected_plane) in citation_cases {
        let citation = CitationResponse {
            label: "E1".into(),
            evidence_id: "ev-1".into(),
            kind: kind.into(),
            role: kind.into(),
            derived_from: None,
            collections: Vec::new(),
            locator: "locator".into(),
            text_preview: "text".into(),
        };
        let taxonomy = ResponseTextTaxonomy::ask_response_with_citations(&[citation]);
        assert_eq!(
            taxonomy
                .fields
                .iter()
                .find(|entry| entry.field == "citations[0].text_preview")
                .unwrap()
                .plane,
            expected_plane,
            "citation kind={kind}"
        );
    }

    let result_cases = [
        ("text", "original_text", OutputTextPlane::Evidence),
        ("image", "image_artifact", OutputTextPlane::Metadata),
        ("text", "image_artifact", OutputTextPlane::GeneratedInterpretation),
        ("unknown", "original_text", OutputTextPlane::GeneratedInterpretation),
        ("ocr", "ocr_text", OutputTextPlane::GeneratedInterpretation),
        ("generated", "image_caption_generated", OutputTextPlane::GeneratedInterpretation),
    ];
    for (kind, role, expected_plane) in result_cases {
        let result = RetrieveResultResponse {
            index: 0,
            rank: 1,
            label: "E1".into(),
            evidence_id: "ev-1".into(),
            text_hash: "hash".into(),
            source_id: "src".into(),
            source_hash: "source-hash".into(),
            source_path: None,
            collections: Vec::new(),
            chunk_id: "chunk".into(),
            kind: kind.into(),
            role: role.into(),
            score: 1.0,
            locator: "locator".into(),
            structured_locator: None,
            provenance: None,
            derived_from: None,
            snippet: "text".into(),
        };
        let taxonomy = ResponseTextTaxonomy::retrieve_response_with_results(&[result]);
        assert_eq!(
            taxonomy
                .fields
                .iter()
                .find(|entry| entry.field == "results[0].snippet")
                .unwrap()
                .plane,
            expected_plane,
            "result kind={kind}, role={role}"
        );
    }
}

#[test]
fn debug_evidence_pack_labels_are_deterministic_interface_text() {
    let value = serde_json::json!({
        "retrieval": {
            "final_evidence_pack": [{"label": "E1"}],
            "display_evidence_pack": [{"label": "E1"}]
        },
        "context": {
            "debug": {
                "final_evidence_pack": [{"label": "E2"}],
                "display_evidence_pack": [{"label": "E2"}]
            }
        }
    });
    let taxonomy = ResponseTextTaxonomy::from_serialized_value(&value);

    for field in [
        "retrieval.final_evidence_pack[].label",
        "retrieval.display_evidence_pack[].label",
        "context.debug.final_evidence_pack[].label",
        "context.debug.display_evidence_pack[].label",
    ] {
        assert_eq!(
            taxonomy
                .fields
                .iter()
                .find(|entry| entry.field == field)
                .unwrap()
                .plane,
            OutputTextPlane::DeterministicInterfaceText,
            "debug label {field}"
        );
    }
}
