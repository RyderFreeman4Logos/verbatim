const CANONICAL_RECORD: &str = r#"{"source_profile":"bible","work_id":"KJV","version_id":"public-domain","components":[{"level":"book","value":"John","ordinal":43},{"level":"chapter","value":"3","ordinal":3},{"level":"verse","value":"16","ordinal":16}],"display_citation":"John 3:16","text":"For God so loved the world."}"#;

fn write_canonical_jsonl(root: &Path, name: &str) -> PathBuf {
    let path = root.join(format!("{name}.jsonl"));
    fs::write(&path, format!("{CANONICAL_RECORD}\n")).unwrap();
    path
}

#[test]
fn issue_246_canonical_parser_ids_are_strict_at_remap_boundary() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = write_canonical_jsonl(tempdir.path(), "fresh");
    let parser_source = SourceId::from_path(&path);
    let catalog_source = SourceId("catalog-source".into());
    let parsed = parser::canonical_jsonl::CanonicalJsonlParser
        .parse(&path)
        .unwrap();
    let stable_id = parsed[0].id.clone();

    let remapped = remap_parser_evidence_identity(parsed, &parser_source, &catalog_source).unwrap();
    assert_eq!(remapped[0].id, stable_id);
    assert_eq!(remapped[0].source_id, catalog_source);

    let digest = "a".repeat(64);
    for bad_id in [
        "unprefixed".into(),
        "cjson:v1:".into(),
        format!("cjson:v1:{}", "a".repeat(63)),
        format!("cjson:v1:{}", "A".repeat(64)),
        format!("cjson:v1:{}", "g".repeat(64)),
        format!("cjson:v2:{digest}"),
        format!("json:v1:{digest}"),
        format!("xcjson:v1:{digest}"),
    ] {
        let bad = synthetic_evidence(&bad_id, &parser_source, 0);
        assert!(
            remap_parser_evidence_identity(vec![bad], &parser_source, &catalog_source).is_err(),
            "accepted malformed self-contained id: {bad_id}"
        );
    }
}

#[tokio::test]
async fn issue_246_canonical_identity_survives_source_relocation() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = write_canonical_jsonl(tempdir.path(), "before");
    let mut pipeline = IngestPipeline::from_parts(
        Store::in_memory().unwrap(),
        HnswIndex::new(),
        RelocationEmbeddingClient,
        tempdir.path().to_path_buf(),
    );
    let source_id = pipeline.add_source(&path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();
    let before = pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap();
    assert_eq!(before.len(), 1);
    assert_eq!(before[0].source_id, source_id);

    let relocated_path = tempdir.path().join("after.jsonl");
    fs::rename(&path, &relocated_path).unwrap();
    pipeline
        .relocate_source(&source_id, &relocated_path)
        .unwrap();

    let after = pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap();
    assert_eq!(after.len(), 1);
    assert_eq!(after[0].id, before[0].id);
    assert_eq!(after[0].source_id, source_id);
}
