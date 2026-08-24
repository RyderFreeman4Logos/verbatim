use super::*;

#[test]
fn evidence_lookup_rejects_report_artifact_ids() {
    let store = Store::in_memory().unwrap();
    let source = source("src");
    store.add_source(&source).unwrap();
    for id in [
        EvidenceId("graphrag://report/community-test".into()),
        EvidenceId("graphrag:report:community-test".into()),
    ] {
        let unit = EvidenceUnit {
            id: id.clone(),
            source_id: source.id.clone(),
            kind: EvidenceKind::Text,
            derived_from: None,
            locator: SourceLocator::Document {
                path_or_url: "/tmp/report.txt".into(),
                line_start: 1,
                line_end: None,
            },
            text: "seeded report row".into(),
            text_hash: "hash-report".into(),
            heading_path: Vec::new(),
            language: None,
            position: 0,
        };
        store
            .bulk_insert_evidence(std::slice::from_ref(&unit))
            .unwrap();

        let err = store.get_evidence(&id).unwrap_err();
        assert!(
            err.to_string()
                .contains("report artifact ids are not evidence"),
            "single lookup must reject report ids, got: {err}"
        );
        let err = store
            .get_evidence_batch(std::slice::from_ref(&id))
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("report artifact ids are not evidence"),
            "batch lookup must reject report ids, got: {err}"
        );
    }

    let chunk = insert_chunk(&store, &source, "chunk-a", "Ordinary evidence.");
    let ordinary = store.get_evidence(&chunk.evidence_unit_ids[0]).unwrap();
    assert!(ordinary.is_some(), "ordinary evidence lookup is unchanged");
}
