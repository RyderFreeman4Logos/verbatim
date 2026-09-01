use std::fs;

use verbatim_core::config::Config;
use verbatim_core::ingest::IngestPipeline;

#[tokio::test]
async fn canonical_jsonl_persists_bounded_annotations() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("annotations.jsonl");
    fs::write(
        &path,
        r#"{"source_profile":"bible","work_id":"KJV","components":[{"level":"book","value":"John","ordinal":43},{"level":"chapter","value":"3","ordinal":3},{"level":"verse","value":"16","ordinal":16}],"text":"For God so loved the world.","metadata":{"annotations":{"note_type":"footnote"}}}"#,
    )
    .unwrap();
    let mut pipeline = IngestPipeline::new(&Config::default(), tempdir.path()).unwrap();
    let source_id = pipeline.add_source(&path).unwrap();

    pipeline.ingest_source(&source_id).await.unwrap();

    let evidence = pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap();
    assert_eq!(evidence.len(), 1);
    assert_eq!(
        serde_json::to_value(&evidence[0]).unwrap()["annotations"],
        serde_json::json!({"note_type": "footnote"})
    );
}

#[test]
fn canonical_jsonl_rejects_oversized_annotations() {
    let tempdir = tempfile::tempdir().unwrap();
    let pipeline = IngestPipeline::new(&Config::default(), tempdir.path()).unwrap();
    let records = [
        r#"{"source_profile":"bible","work_id":"KJV","components":[{"level":"book","value":"John"}],"text":"text","metadata":{"annotations":{"a":"1","b":"2","c":"3","d":"4","e":"5","f":"6","g":"7","h":"8","i":"9"}}}"#.to_string(),
        format!(
            r#"{{"source_profile":"bible","work_id":"KJV","components":[{{"level":"book","value":"John"}}],"text":"text","metadata":{{"annotations":{{"note_type":"{}"}}}}}}"#,
            "x".repeat(65)
        ),
    ];

    for (index, record) in records.iter().enumerate() {
        let path = tempdir.path().join(format!("oversized-{index}.jsonl"));
        fs::write(&path, record).unwrap();

        let error = pipeline.add_source(&path).unwrap_err().to_string();
        assert!(error.contains("annotations"), "error was: {error}");
        assert!(pipeline.store().list_sources().unwrap().is_empty());
    }
}
