use super::*;

#[tokio::test]
async fn unsupported_filter_image_only_pdf_rejects_without_ocr() {
    let tempdir = tempfile::tempdir().unwrap();
    let ocr = MockOcrProvider::new("eng", "default");
    let mut pipeline = IngestPipeline::from_parts(
        Store::in_memory().unwrap(),
        HnswIndex::new(),
        StaticEmbeddingClient,
        tempdir.path().to_path_buf(),
    )
    .with_ocr_provider(ocr.clone());
    let path = tempdir.path().join("unsupported-image-only.pdf");
    write_pdf_with_unsupported_image(&path);
    let source_id = pipeline.add_source(&path).unwrap();

    let error = pipeline
        .ingest_source(&source_id)
        .await
        .expect_err("unsupported image filters must not bypass image-only rejection");

    assert_eq!(
        error.downcast_ref::<IngestDiagnosticCode>(),
        Some(&IngestDiagnosticCode::PdfNoUsableTextLayer)
    );
    assert_eq!(error.to_string(), "pdf_no_usable_text_layer");
    assert_eq!(ocr.call_count(), 0);
    assert!(pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap()
        .is_empty());
}

#[tokio::test]
async fn mixed_pdf_ocr_output_stays_out_of_persistent_evidence() {
    let tempdir = tempfile::tempdir().unwrap();
    let ocr = MockOcrProvider::new("eng", "default");
    let mut pipeline = IngestPipeline::from_parts(
        Store::in_memory().unwrap(),
        HnswIndex::new(),
        StaticEmbeddingClient,
        tempdir.path().to_path_buf(),
    )
    .with_ocr_provider(ocr.clone());
    let path = tempdir.path().join("mixed-text-and-image.pdf");
    write_pdf_with_text_and_image_only_pages(&path);
    let source_id = pipeline.add_source(&path).unwrap();

    pipeline.ingest_source(&source_id).await.unwrap();

    let evidence = pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap();
    assert_eq!(ocr.call_count(), 1);
    assert!(evidence
        .iter()
        .any(|unit| unit.kind == EvidenceKind::Text && unit.text.contains("Mixed PDF text")));
    assert!(evidence.iter().all(|unit| unit.kind != EvidenceKind::Ocr));
    assert!(pipeline
        .store()
        .list_chunks_by_source(&source_id)
        .unwrap()
        .iter()
        .all(|chunk| !chunk.text.contains("ocrneedle")));
    assert_eq!(
        pipeline.source_ingest_freshness(&source_id).unwrap(),
        SourceIngestFreshness::Fresh
    );
    assert!(pipeline.check_stale().unwrap().is_empty());
}

fn write_pdf_with_unsupported_image(path: &std::path::Path) {
    let image_prefix = b"<< /Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceRGB /BitsPerComponent 8 /Filter /FlateDecode";
    let content = b"q\n36 0 0 36 72 72 cm\n/Im1 Do\nQ\n";
    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject << /Im1 4 0 R >> >> /Contents 5 0 R >>".to_vec(),
        stream_object(image_prefix, &[120, 156, 3, 0, 0, 0, 0, 1]),
        stream_object(b"<<", content),
    ];
    std::fs::write(path, pdf_bytes(objects)).unwrap();
}

fn write_pdf_with_text_and_image_only_pages(path: &std::path::Path) {
    let text = b"BT\n/F1 12 Tf\n72 120 Td\n(Mixed PDF text remains source evidence) Tj\nET\n";
    let image = vec![255_u8; 8 * 8 * 3];
    let image_content = b"q\n36 0 0 36 72 72 cm\n/Im1 Do\nQ\n";
    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R 4 0 R] /Count 2 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 5 0 R >> >> /Contents 6 0 R >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /XObject << /Im1 7 0 R >> >> /Contents 8 0 R >>".to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        stream_object(b"<<", text),
        stream_object(
            b"<< /Type /XObject /Subtype /Image /Width 8 /Height 8 /ColorSpace /DeviceRGB /BitsPerComponent 8",
            &image,
        ),
        stream_object(b"<<", image_content),
    ];
    std::fs::write(path, pdf_bytes(objects)).unwrap();
}
