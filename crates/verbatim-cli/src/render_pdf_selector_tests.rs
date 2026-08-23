use super::*;

#[test]
fn pdf_evidence_lookup_renders_selector_details_and_preserves_legacy_output() {
    let selector = verbatim_core::pdf_selector::PdfSelector::from_page_range(
        "a".repeat(64),
        "pdf_oxide",
        "prefix exact suffix",
        7..12,
    )
    .unwrap();
    let mut response = EvidenceResponse {
        id: "ev-pdf".into(),
        source_id: "src-1".into(),
        text_taxonomy: verbatim_core::api::ResponseTextTaxonomy::evidence_response(true),
        source_hash: Some("persisted-source-hash".into()),
        source_bounded: true,
        text_hash: selector.selected_text_hash.clone(),
        kind: "text".into(),
        derived_from: None,
        locator: "PDF p.3, para 2".into(),
        structured_locator: SourceLocator::Pdf {
            page: 3,
            paragraph: 2,
            bbox: None,
            selector: Some(selector.clone()),
        },
        text: "exact".into(),
        heading_path: Vec::new(),
        language: None,
        position: 2,
        image_artifact: None,
    };
    let mut output = Vec::new();

    write_evidence(&mut output, &response).unwrap();
    let output = String::from_utf8(output).unwrap();

    assert!(output.contains("pdf_selector: {"));
    assert!(output.contains("\"version\":1"));
    assert!(output.contains("\"normalization_profile\":\"unicode_whitespace_v1\""));
    assert!(output.contains(&format!("\"source_hash\":\"{}\"", selector.source_hash)));
    assert!(output.contains("\"parser_profile_id\":\"pdf_oxide\""));
    assert!(output.contains(&format!(
        "\"page_text_hash\":\"{}\"",
        selector.page_text_hash
    )));
    assert!(output.contains("\"position\":{\"start\":7,\"end\":12}"));
    assert!(output.contains("\"quote\":{\"exact\":\"exact\""));
    assert!(output.contains(&format!(
        "\"selected_text_hash\":\"{}\"",
        selector.selected_text_hash
    )));

    response.locator = "PDF p.3, para 2 (legacy anchor)".into();
    response.structured_locator = SourceLocator::legacy_pdf(3, 2, None);
    let mut legacy_output = Vec::new();
    write_evidence(&mut legacy_output, &response).unwrap();
    let legacy_output = String::from_utf8(legacy_output).unwrap();
    assert!(legacy_output.contains("locator: PDF p.3, para 2 (legacy anchor)"));
    assert!(!legacy_output.contains("pdf_selector:"));
}
