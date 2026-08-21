use super::*;

#[test]
fn source_bounded_retrieval_batches_evidence_eligibility_queries() {
    const LARGE_CANDIDATE_COUNT: usize = 16;
    let all_results = (0..LARGE_CANDIDATE_COUNT)
        .map(|index| {
            test_retrieval_result(
                index + 1,
                &format!("chunk-{index:02}"),
                &format!("evidence-{index:02}"),
                EvidenceKind::Text,
            )
        })
        .collect::<Vec<_>>();
    let dir = persist_retrieval_filter_fixture("batch-eligibility", &all_results);
    let store = Store::open_existing_readonly(&dir.path().join("verbatim.db")).unwrap();

    let statement_count = |candidate_count| {
        let mut results = all_results[..candidate_count].to_vec();
        let mut debug = empty_retrieval_debug();
        let (filtered, count) = store.count_sql_statements(|| {
            filter_generated_retrieval_evidence(&store, &mut results, Some(&mut debug), false)
        });
        filtered.unwrap();
        assert_eq!(results.len(), candidate_count);
        count.unwrap()
    };

    assert_eq!(
        (statement_count(1), statement_count(LARGE_CANDIDATE_COUNT)),
        (3, 3),
        "eligibility lookup must stay at two chunk batches plus one evidence batch"
    );
}

#[test]
fn source_bounded_retrieval_fails_closed_per_invalid_persisted_evidence() {
    let mut results = vec![
        test_retrieval_result(1, "valid-1", "valid-evidence-1", EvidenceKind::Text),
        test_retrieval_result(
            2,
            "generated",
            "generated-evidence",
            EvidenceKind::Generated,
        ),
        test_retrieval_result(3, "missing", "missing-evidence", EvidenceKind::Text),
        test_retrieval_result(4, "corrupt", "corrupt-evidence", EvidenceKind::Text),
        test_retrieval_result(5, "valid-2", "valid-evidence-2", EvidenceKind::Text),
    ];
    let dir = persist_retrieval_filter_fixture("invalid-evidence", &results);
    let database_path = dir.path().join("verbatim.db");
    let connection = rusqlite::Connection::open(&database_path).unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = OFF;")
        .unwrap();
    connection
        .execute(
            "DELETE FROM evidence_units WHERE id = ?1",
            ["missing-evidence"],
        )
        .unwrap();
    connection
        .execute(
            "UPDATE evidence_units SET locator_json = '{' WHERE id = ?1",
            ["corrupt-evidence"],
        )
        .unwrap();
    drop(connection);
    let store = Store::open_existing_readonly(&database_path).unwrap();
    let mut debug = empty_retrieval_debug();
    debug.bm25_hits = results
        .iter()
        .map(|result| verbatim_core::types::RetrievalStageHit {
            rank: result.provenance.result_rank,
            chunk_id: result.chunk_id.clone(),
            source_id: Some(result.chunk.source_id.clone()),
            score: result.score,
            evidence_ids: result.chunk.evidence_unit_ids.clone(),
        })
        .collect();

    filter_generated_retrieval_evidence(&store, &mut results, Some(&mut debug), true)
        .expect("invalid evidence is candidate-local");

    assert_eq!(
        results
            .iter()
            .map(|result| (result.chunk_id.0.as_str(), result.provenance.result_rank))
            .collect::<Vec<_>>(),
        [("valid-1", 1), ("valid-2", 2)]
    );
    assert_eq!(
        debug
            .bm25_hits
            .iter()
            .map(|hit| (hit.chunk_id.0.as_str(), hit.rank))
            .collect::<Vec<_>>(),
        [("valid-1", 1), ("valid-2", 2)]
    );
}

fn persist_retrieval_filter_fixture(name: &str, results: &[RetrievalResult]) -> TestDir {
    let dir = TestDir::new(&format!("source-bounded-filter-{name}"));
    let store = Store::new(&dir.path().join("verbatim.db")).unwrap();
    store
        .add_source(&Source {
            id: SourceId("src".into()),
            path: dir.path().join("source.md"),
            hash: "source-hash".into(),
            status: SourceStatus::Indexed,
            parser_used: Some("markdown".into()),
            last_ingested_at: None,
        })
        .unwrap();
    let evidence = results
        .iter()
        .flat_map(|result| result.evidence_units.clone())
        .collect::<Vec<_>>();
    let chunks = results
        .iter()
        .map(|result| result.chunk.clone())
        .collect::<Vec<_>>();
    let links = results
        .iter()
        .flat_map(|result| {
            result
                .chunk
                .evidence_unit_ids
                .iter()
                .map(|id| (result.chunk_id.clone(), id.clone()))
        })
        .collect::<Vec<_>>();
    store.bulk_insert_evidence(&evidence).unwrap();
    store.bulk_insert_chunks(&chunks).unwrap();
    store.link_chunk_evidence(&links).unwrap();
    dir
}

#[test]
fn source_bounded_retrieval_omits_ocr_evidence_without_debug() {
    let store = Store::in_memory().unwrap();
    let mut results = vec![test_retrieval_result(
        1,
        "ocr-chunk",
        "ocr-evidence",
        EvidenceKind::Ocr,
    )];
    let result = &results[0];
    store
        .add_source(&Source {
            id: result.evidence_units[0].source_id.clone(),
            path: "ocr.pdf".into(),
            hash: "source-hash".into(),
            status: SourceStatus::Indexed,
            parser_used: Some("test".into()),
            last_ingested_at: None,
        })
        .expect("OCR source persists for the negative retrieval fixture");
    store
        .bulk_insert_evidence(&result.evidence_units)
        .expect("OCR evidence persists for the negative retrieval fixture");
    store
        .bulk_insert_chunks(std::slice::from_ref(&result.chunk))
        .expect("OCR chunk persists for the negative retrieval fixture");
    store
        .link_chunk_evidence(&[(result.chunk.id.clone(), result.evidence_units[0].id.clone())])
        .expect("OCR chunk links to its evidence");

    filter_generated_retrieval_evidence(&store, &mut results, None, false)
        .expect("source-bounded retrieval filters OCR evidence");
    assert!(results.is_empty());
}
