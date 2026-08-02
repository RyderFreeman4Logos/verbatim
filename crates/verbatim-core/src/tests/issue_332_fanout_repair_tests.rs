#[cfg(feature = "parser-pdfplumber")]
fn issue_332_write_text_pdf(path: &Path, text: &str) {
    let content = format!("BT\n/F1 12 Tf\n72 120 Td\n({text}) Tj\nET\n");
    let objects = vec![
        b"<< /Type /Catalog /Pages 2 0 R >>".to_vec(),
        b"<< /Type /Pages /Kids [3 0 R] /Count 1 >>".to_vec(),
        b"<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] /Resources << /Font << /F1 4 0 R >> >> /Contents 5 0 R >>".to_vec(),
        b"<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>".to_vec(),
        issue_332_pdf_stream(content.as_bytes()),
    ];
    let mut pdf = b"%PDF-1.7\n".to_vec();
    let mut offsets = Vec::new();
    for (index, object) in objects.iter().enumerate() {
        offsets.push(pdf.len());
        pdf.extend(format!("{} 0 obj\n", index + 1).as_bytes());
        pdf.extend(object);
        pdf.extend(b"\nendobj\n");
    }
    let xref_offset = pdf.len();
    pdf.extend(format!("xref\n0 {}\n0000000000 65535 f \n", objects.len() + 1).as_bytes());
    for offset in offsets {
        pdf.extend(format!("{offset:010} 00000 n \n").as_bytes());
    }
    pdf.extend(
        format!(
            "trailer\n<< /Size {} /Root 1 0 R >>\nstartxref\n{xref_offset}\n%%EOF\n",
            objects.len() + 1
        )
        .as_bytes(),
    );
    fs::write(path, pdf).unwrap();
}

#[cfg(feature = "parser-pdfplumber")]
fn issue_332_pdf_stream(data: &[u8]) -> Vec<u8> {
    let mut object = format!("<< /Length {} >>\nstream\n", data.len()).into_bytes();
    object.extend(data);
    object.extend(b"\nendstream");
    object
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn issue_332_relocation_rejects_commit_front_ancestor_symlink_swap() {
    use std::os::unix::fs::symlink;

    let tempdir = tempfile::tempdir().unwrap();
    let live = tempdir.path().join("live");
    let parked = tempdir.path().join("parked");
    fs::create_dir(&live).unwrap();
    let (mut pipeline, source_id, old_path) = indexed_fixture(&live, "old", false).await;
    let target = live.join("target.txt");
    fs::rename(&old_path, &target).unwrap();
    let before = catalog_snapshot(&pipeline, &source_id);
    let live_for_hook = live.clone();
    pipeline
        .store()
        .set_source_relocation_before_mutation_hook(move || {
            fs::rename(&live_for_hook, &parked).unwrap();
            symlink(&parked, &live_for_hook).unwrap();
        });

    let error = pipeline.relocate_source(&source_id, &target).unwrap_err();

    assert_eq!(
        crate::store::source_relocation_error_kind(&error),
        Some(crate::store::SourceRelocationErrorKind::Validation)
    );
    assert_eq!(catalog_snapshot(&pipeline, &source_id), before);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn issue_332_relocation_classifies_enotdir_for_target_ancestor_file() {
    let tempdir = tempfile::tempdir().unwrap();
    let (mut pipeline, source_id, old_path) =
        indexed_fixture(tempdir.path(), "target-ancestor-file", false).await;
    let ancestor = tempdir.path().join("ancestor");
    fs::write(&ancestor, "not a directory").unwrap();
    let target = ancestor.join("target.txt");
    fs::remove_file(old_path).unwrap();
    let before = catalog_snapshot(&pipeline, &source_id);

    let error = pipeline.relocate_source(&source_id, &target).unwrap_err();

    assert_eq!(
        crate::store::source_relocation_error_kind(&error),
        Some(crate::store::SourceRelocationErrorKind::Validation)
    );
    assert_eq!(catalog_snapshot(&pipeline, &source_id), before);
}

#[cfg(target_os = "linux")]
#[tokio::test]
async fn issue_332_relocation_classifies_enotdir_for_stored_path_ancestor_file() {
    let tempdir = tempfile::tempdir().unwrap();
    let live = tempdir.path().join("live");
    let parked = tempdir.path().join("parked");
    fs::create_dir(&live).unwrap();
    let (mut pipeline, source_id, old_path) =
        indexed_fixture(&live, "stored-path-ancestor-file", false).await;
    let target = live.join("target.txt");
    fs::rename(&old_path, &target).unwrap();
    let before = catalog_snapshot(&pipeline, &source_id);
    let live_for_hook = live.clone();
    pipeline
        .store()
        .set_source_relocation_before_mutation_hook(move || {
            fs::rename(live_for_hook, parked).unwrap();
            fs::write(live, "not a directory").unwrap();
        });

    let error = pipeline.relocate_source(&source_id, &target).unwrap_err();

    assert_eq!(
        crate::store::source_relocation_error_kind(&error),
        Some(crate::store::SourceRelocationErrorKind::Validation)
    );
    assert_eq!(catalog_snapshot(&pipeline, &source_id), before);
}

#[cfg(feature = "parser-pdfplumber")]
#[tokio::test]
async fn issue_332_relocation_replays_recorded_pdfplumber_through_held_snapshot() {
    let tempdir = tempfile::tempdir().unwrap();
    let old_path = tempdir.path().join("before.pdf");
    let new_path = tempdir.path().join("after.pdf");
    let held_path = tempdir.path().join("held.pdf");
    issue_332_write_text_pdf(&old_path, "Recorded pdfplumber relocation evidence");
    let evidence =
        crate::traits::Parser::parse(&crate::parser::plumber::PdfPlumberParser, &old_path).unwrap();
    assert!(!evidence.is_empty());
    let source_id = SourceId::from_path(&old_path);
    assert!(evidence.iter().all(|unit| unit.source_id == source_id));
    let source = Source {
        id: source_id.clone(),
        path: fs::canonicalize(&old_path).unwrap(),
        hash: crate::types::hex_sha256(&fs::read(&old_path).unwrap()),
        status: SourceStatus::Indexed,
        parser_used: Some("pdfplumber".into()),
        last_ingested_at: None,
    };
    let store = Store::in_memory().unwrap();
    store.add_source(&source).unwrap();
    for unit in &evidence {
        store
            .connection()
            .execute(
                "INSERT INTO evidence_units
                 (id, source_id, kind, locator_json, text, text_hash, heading_path_json,
                  position, derived_from_evidence_id)
                 VALUES (?1, ?2, 'Text', ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    &unit.id.0,
                    &unit.source_id.0,
                    serde_json::to_string(&unit.locator).unwrap(),
                    &unit.text,
                    &unit.text_hash,
                    serde_json::to_string(&unit.heading_path).unwrap(),
                    unit.position,
                    unit.derived_from.as_ref().map(|id| &id.0),
                ],
            )
            .unwrap();
    }
    let mut nodes = vec![GraphNode {
        id: GraphNodeId::new(&source_id, GraphNodeKind::Source, &source_id.0),
        source_id: source_id.clone(),
        kind: GraphNodeKind::Source,
        external_id: source_id.0.clone(),
        label: Some("before.pdf".into()),
        locator: None,
        ordinal: None,
        metadata: Some(serde_json::json!({
            "path": source.path.to_string_lossy(),
            "hash": &source.hash,
            "parser_used": &source.parser_used,
        })),
    }];
    nodes.extend(evidence.iter().map(|unit| GraphNode {
        id: GraphNodeId::new(&source_id, GraphNodeKind::EvidenceUnit, &unit.id.0),
        source_id: source_id.clone(),
        kind: GraphNodeKind::EvidenceUnit,
        external_id: unit.id.0.clone(),
        label: Some(format!("text evidence {}", unit.position)),
        locator: Some(unit.locator.clone()),
        ordinal: Some(unit.position),
        metadata: Some(serde_json::json!({
            "kind": "text",
            "text_hash": &unit.text_hash,
            "heading_path": &unit.heading_path,
        })),
    }));
    store.upsert_graph_nodes(&nodes).unwrap();
    let mut pipeline = IngestPipeline::from_parts(
        store,
        HnswIndex::new(),
        RelocationEmbeddingClient,
        tempdir.path().to_path_buf(),
    );
    fs::rename(&old_path, &new_path).unwrap();
    let target_before_parse = new_path.clone();
    let target_after_parse = new_path.clone();
    let held_before_parse = held_path.clone();
    let held_after_parse = held_path;
    pipeline.store().set_source_relocation_parse_hooks(
        move || fs::rename(target_before_parse, held_before_parse).unwrap(),
        move || fs::rename(held_after_parse, target_after_parse).unwrap(),
    );

    let relocated = pipeline.relocate_source(&source_id, &new_path).unwrap();

    assert_eq!(relocated.id, source_id);
    assert_eq!(relocated.parser_used.as_deref(), Some("pdfplumber"));
    assert_eq!(relocated.path, fs::canonicalize(&new_path).unwrap());
}

#[tokio::test]
async fn issue_332_relocation_into_collection_root_preserves_catalog() {
    let tempdir = tempfile::tempdir().unwrap();
    let standalone = tempdir.path().join("standalone");
    let collection_root = tempdir.path().join("collection-root");
    fs::create_dir(&standalone).unwrap();
    fs::create_dir(&collection_root).unwrap();
    let (mut pipeline, source_id, old_path) = indexed_fixture(&standalone, "old", false).await;
    pipeline.store().create_collection("docs", &[]).unwrap();
    pipeline
        .store()
        .add_collection_root("docs", &collection_root)
        .unwrap();
    let target = collection_root.join("target.txt");
    fs::rename(&old_path, &target).unwrap();
    let before = catalog_snapshot(&pipeline, &source_id);

    let error = pipeline.relocate_source(&source_id, &target).unwrap_err();

    assert!(format!("{error:#}").contains("collection root"));
    assert_eq!(catalog_snapshot(&pipeline, &source_id), before);
    assert!(pipeline
        .store()
        .list_collection_members("docs")
        .unwrap()
        .is_empty());
}
