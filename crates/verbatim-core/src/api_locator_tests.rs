use super::*;
use crate::types::{MarkdownBlockKind, MarkdownHeadingLocator, SourceLocator};

#[test]
fn markdown_locator_serializes_in_evidence_and_retrieve_responses() {
    let locator = SourceLocator::Markdown {
        path: "/tmp/doc.md".into(),
        line_start: 3,
        line_end: 5,
        byte_start: 24,
        byte_end: 96,
        block_kind: MarkdownBlockKind::BlockQuote,
        block_index: 1,
        block_hash: "stable-block-hash".into(),
        heading_level: Some(2),
        heading_slug: Some("details-1".into()),
        heading_path: vec![
            MarkdownHeadingLocator {
                level: 1,
                text: "Intro".into(),
                slug: "intro".into(),
                line: 1,
            },
            MarkdownHeadingLocator {
                level: 2,
                text: "Details".into(),
                slug: "details-1".into(),
                line: 2,
            },
        ],
    };
    let evidence = EvidenceResponse {
        id: "ev-md".into(),
        source_id: "src-1".into(),
        text_taxonomy: ResponseTextTaxonomy::evidence_response(true),
        source_hash: Some("persisted-source-hash".into()),
        source_bounded: true,
        text_hash: "verified-text-hash".into(),
        kind: "text".into(),
        derived_from: None,
        locator: locator.to_string(),
        structured_locator: locator.clone(),
        text: "quoted markdown".into(),
        heading_path: vec!["Intro".into(), "Details".into()],
        language: None,
        position: 1,
        image_artifact: None,
    };
    let retrieve = RetrieveResultResponse {
        index: 0,
        rank: 1,
        label: "E1".into(),
        evidence_id: "ev-md".into(),
        text_hash: "verified-text-hash".into(),
        source_id: "src-1".into(),
        source_hash: "persisted-source-hash".into(),
        source_path: Some("/tmp/doc.md".into()),
        collections: Vec::new(),
        chunk_id: "chunk-1".into(),
        kind: "text".into(),
        role: "original_text".into(),
        score: 0.03,
        locator: evidence.locator.clone(),
        structured_locator: Some(locator),
        provenance: None,
        derived_from: None,
        snippet: "quoted markdown".into(),
    };

    let evidence_json = serde_json::to_value(&evidence).unwrap();
    let retrieve_json = serde_json::to_value(&retrieve).unwrap();

    assert_eq!(evidence_json["structured_locator"]["type"], "Markdown");
    assert_eq!(
        evidence_json["structured_locator"]["block_kind"],
        "block_quote"
    );
    assert_eq!(
        evidence_json["structured_locator"]["heading_path"][1]["slug"],
        "details-1"
    );
    assert_eq!(retrieve_json["structured_locator"]["type"], "Markdown");
    assert_eq!(
        retrieve_json["structured_locator"]["block_hash"],
        "stable-block-hash"
    );
}
