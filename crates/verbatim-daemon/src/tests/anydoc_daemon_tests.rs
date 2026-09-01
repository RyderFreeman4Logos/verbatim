use super::*;

fn write_text_layer_pdf(path: &std::path::Path) {
    let content = b"BT\n/F1 12 Tf\n72 120 Td\n(AnyDoc daemon text layer.) Tj\nET\n";
    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        pdf_stream_object(b"<<", content),
    ];
    std::fs::write(path, pdf_bytes(objects)).expect("PDF text-layer fixture writes");
}

#[tokio::test]
async fn daemon_ingest_records_anydoc_identity_for_text_layer_pdf() {
    let test_dir = TestDir::new("daemon-anydoc-text-layer");
    let source_path = test_dir.path().join("text-layer.pdf");
    write_text_layer_pdf(&source_path);
    let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
    config.embedding.enabled = false;
    config.rerank.enabled = false;
    let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
    let source_id = pipeline.add_source(&source_path).unwrap();
    let state = test_state(config, test_dir.path(), pipeline);

    let Json(response) = ingest_all(
        State(Arc::clone(&state)),
        Query(IngestQuery {
            force: false,
            embedding_profile_id: None,
            vectors_only: false,
        }),
    )
    .await
    .unwrap();
    assert_eq!(response.ingested, 1);

    let Json(source) = get_source(State(Arc::clone(&state)), Path(source_id.0.clone()))
        .await
        .unwrap();
    assert_eq!(source.parser_used.as_deref(), Some("anydoc_pdf"));

    let Json(tasks) = list_tasks_handler(
        State(Arc::clone(&state)),
        Query(TaskListQuery {
            status: Some("all".into()),
            limit: Some(10),
        }),
    )
    .await
    .unwrap();
    let task_id = tasks
        .tasks
        .iter()
        .find(|task| task.kind == TaskKind::Ingest)
        .expect("text-layer ingest task")
        .id
        .clone();
    let summary = task_summary_response(&state, task_id).await.unwrap();
    let parse_span = summary
        .spans
        .iter()
        .find(|span| span.phase == "parse")
        .expect("parse phase timing");
    assert_eq!(
        parse_span.metadata["conversion"]["converter"],
        "anydoc+pdf-inspector"
    );
    assert_eq!(
        parse_span.metadata["conversion"]["converter_version"],
        "anydoc@0.2.4;pdf-inspector@1.14.2"
    );
    assert!(!parse_span.metadata["conversion"]["output_hash"]
        .as_str()
        .expect("AnyDoc output hash")
        .is_empty());
}
