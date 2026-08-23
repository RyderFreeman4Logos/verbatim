#[test]
fn markdown_evidence_lookup_renders_structured_locator_details() {
    let response = EvidenceResponse {
        id: "ev-md".into(),
        source_id: "src-1".into(),
        text_taxonomy: ResponseTextTaxonomy::evidence_response(true),
        source_hash: Some("persisted-source-hash".into()),
        source_bounded: true,
        text_hash: "receipt-text-hash".into(),
        kind: "text".into(),
        derived_from: None,
        locator: "/tmp/doc.md L3 markdown:paragraph #intro".into(),
        structured_locator: SourceLocator::Markdown {
            path: "/tmp/doc.md".into(),
            line_start: 3,
            line_end: 4,
            byte_start: 24,
            byte_end: 86,
            block_kind: MarkdownBlockKind::Paragraph,
            block_index: 2,
            block_hash: "block-hash".into(),
            heading_level: Some(1),
            heading_slug: Some("intro".into()),
            heading_path: vec![MarkdownHeadingLocator {
                level: 1,
                text: "Intro".into(),
                slug: "intro".into(),
                line: 1,
            }],
        },
        text: "markdown evidence text".into(),
        heading_path: vec!["Intro".into()],
        language: None,
        position: 2,
        image_artifact: None,
    };
    let mut output = Vec::new();

    write_evidence(&mut output, &response).unwrap();
    let output = String::from_utf8(output).unwrap();

    assert!(output.contains("  source_bounded: true"));
    assert!(output.contains("  source_hash: persisted-source-hash"));
    assert!(output.contains("  text_hash: receipt-text-hash"));
    assert!(output.contains("markdown_locator:"));
    assert!(output.contains("block_kind: paragraph"));
    assert!(output.contains("line_range: 3-4"));
    assert!(output.contains("byte_range: 24-86"));
    assert!(output.contains("block_hash: block-hash"));
    assert!(output.contains("heading_slug: intro"));
    assert!(output.contains("heading: level=1 line=1 slug=intro text=Intro"));
}

#[test]
fn retrieve_debug_output_renders_markdown_structured_locator_when_present() {
    let response = RetrieveResponse {
        task_id: "task-1".into(),
        query: "markdown".into(),
        text_taxonomy: ResponseTextTaxonomy::retrieve_response(),
        source_id: None,
        collection_filter: None,
        embedding_profile_id: "default".into(),
        limit: 1,
        page_size: 1,
        page: 1,
        total_results: 1,
        returned_results: 1,
        source_bounded: true,
        controls: RetrieveControlsResponse {
            fast: false,
            rerank_enabled: false,
            dense_top_k: 10,
            bm25_top_k: 10,
            rrf_k: 60,
            rerank_top_n: 1,
        },
        audit_receipt: AuditReceipt {
            version: AUDIT_RECEIPT_VERSION,
            embedding_profile_id: "default".into(),
            source_bounded: true,
            controls: RetrieveControlsResponse {
                fast: false,
                rerank_enabled: false,
                dense_top_k: 10,
                bm25_top_k: 10,
                rrf_k: 60,
                rerank_top_n: 1,
            },
            results: vec![AuditReceiptResult {
                evidence_id: "ev-md".into(),
                text_hash: "verified-text-hash".into(),
                source_hash: "persisted-source-hash".into(),
            }],
        },
        timings: Vec::new(),
        results: vec![RetrieveResultResponse {
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
            score: 0.42,
            locator: "/tmp/doc.md L3 markdown:paragraph #intro".into(),
            structured_locator: Some(SourceLocator::Markdown {
                path: "/tmp/doc.md".into(),
                line_start: 3,
                line_end: 3,
                byte_start: 24,
                byte_end: 48,
                block_kind: MarkdownBlockKind::Paragraph,
                block_index: 0,
                block_hash: "block-hash".into(),
                heading_level: Some(1),
                heading_slug: Some("intro".into()),
                heading_path: vec![MarkdownHeadingLocator {
                    level: 1,
                    text: "Intro".into(),
                    slug: "intro".into(),
                    line: 1,
                }],
            }),
            provenance: None,
            derived_from: None,
            snippet: "markdown evidence".into(),
        }],
        debug: None,
    };
    let mut output = Vec::new();

    write_retrieve_debug_response(&mut output, &response).unwrap();
    let output = String::from_utf8(output).unwrap();

    assert!(output.contains("locator: /tmp/doc.md L3 markdown:paragraph #intro"));
    assert!(output.contains("source_hash: persisted-source-hash"));
    assert!(output.contains("markdown_locator:"));
    assert!(output.contains("block_hash: block-hash"));
}
