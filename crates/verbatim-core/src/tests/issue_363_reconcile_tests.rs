use super::*;
use crate::deletion::{DeletionOutcome, DeletionReport, RetentionPolicy};
use crate::types::{Source, SourceStatus};
use async_trait::async_trait;

struct ReconcileEmbeddingClient;

#[async_trait]
impl EmbeddingClient for ReconcileEmbeddingClient {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }

    fn dimension(&self) -> usize {
        2
    }
}

#[test]
fn bounded_reconcile_selects_never_attempted_qdrant_tombstone_after_terminal_and_held_prefix() {
    const STARTUP_BATCH_CAP: usize = 16;

    let store = Store::in_memory().unwrap();
    for (prefix, retention_policy) in [
        ("terminal", RetentionPolicy::Immediate),
        ("held", RetentionPolicy::LegalHold),
    ] {
        for index in 0..STARTUP_BATCH_CAP / 2 {
            let source = Source {
                id: SourceId(format!("a-{prefix}-{index:02}")),
                path: std::path::PathBuf::from(format!("a-{prefix}-{index:02}.md")),
                hash: "test-hash".into(),
                status: SourceStatus::Pending,
                parser_used: None,
                last_ingested_at: None,
            };
            store.add_source(&source).unwrap();
            store
                .remove_source_with_retention(&source.id, retention_policy)
                .unwrap();
            let mut report = DeletionReport::new();
            let mut transaction = store.connection().unchecked_transaction().unwrap();
            store
                .finalize_deletion_outcome_tx(
                    &mut transaction,
                    &source.id,
                    DeletionOutcome::Erased,
                    &mut report,
                )
                .unwrap();
            transaction.commit().unwrap();
        }
    }
    let later_pending = Source {
        id: SourceId("z-later-pending-qdrant-tombstone".into()),
        path: std::path::PathBuf::from("z-later-pending-qdrant-tombstone.md"),
        hash: "test-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    store.add_source(&later_pending).unwrap();
    store.remove_source(&later_pending.id).unwrap();

    let selected = store
        .reconciliation_deletion_source_ids_up_to(STARTUP_BATCH_CAP)
        .unwrap();

    assert_eq!(selected.first(), Some(&later_pending.id));
    assert!(selected.contains(&later_pending.id));
}

#[test]
fn bounded_reconcile_query_avoids_large_deletion_report_history() {
    const BATCH_SIZE: usize = 3;
    const REPORT_HISTORY_ROWS: usize = 4_096;

    let store = Store::in_memory().unwrap();
    let terminal = Source {
        id: SourceId("terminal-history".into()),
        path: std::path::PathBuf::from("terminal-history.md"),
        hash: "test-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    store.add_source(&terminal).unwrap();
    store.remove_source(&terminal.id).unwrap();
    let mut report = DeletionReport::new();
    let mut transaction = store.connection().unchecked_transaction().unwrap();
    store
        .finalize_deletion_outcome_tx(
            &mut transaction,
            &terminal.id,
            DeletionOutcome::Erased,
            &mut report,
        )
        .unwrap();
    transaction.commit().unwrap();
    for _ in 0..REPORT_HISTORY_ROWS {
        store
            .persist_deletion_report(&terminal.id, RetentionPolicy::Immediate, &report)
            .unwrap();
    }

    let pending = (0..BATCH_SIZE + 1)
        .map(|index| Source {
            id: SourceId(format!("pending-{index:02}")),
            path: std::path::PathBuf::from(format!("pending-{index:02}.md")),
            hash: "test-hash".into(),
            status: SourceStatus::Pending,
            parser_used: None,
            last_ingested_at: None,
        })
        .collect::<Vec<_>>();
    for source in &pending {
        store.add_source(source).unwrap();
        store.remove_source(&source.id).unwrap();
    }

    let selected = store
        .reconciliation_deletion_source_ids_up_to(BATCH_SIZE)
        .unwrap();
    assert_eq!(
        selected,
        pending[..BATCH_SIZE]
            .iter()
            .map(|source| source.id.clone())
            .collect::<Vec<_>>()
    );

    let mut statement = store
        .connection()
        .prepare(
            "EXPLAIN QUERY PLAN
             SELECT source_id
             FROM source_tombstones
             WHERE (
                     qdrant_outcome = 'pending'
                     OR legal_hold = 1
                     OR backup_expiry_at IS NOT NULL
                   )
               AND (
                     qdrant_outcome = 'pending'
                     OR legal_hold = 1
                     OR last_reconcile_attempt_ts IS NULL
                     OR last_reconcile_attempt_ts < backup_expiry_at
                   )
             ORDER BY last_reconcile_attempt_seq, source_id
             LIMIT 3",
        )
        .unwrap();
    let query_plan = statement
        .query_map([], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<std::result::Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");
    assert!(query_plan.contains("source_tombstones_reconcile_attempt_idx"));
    assert!(!query_plan.contains("deletion_reports"));
}

#[tokio::test]
async fn bounded_reconcile_round_robins_pending_tombstones_through_same_timestamp_ties() {
    const BATCH_SIZE: usize = 2;

    let tempdir = tempfile::tempdir().unwrap();
    let store = Store::in_memory().unwrap();
    let source_ids = (0..BATCH_SIZE * 2)
        .map(|index| SourceId(format!("round-robin-pending-{index:02}")))
        .collect::<Vec<_>>();
    for source_id in &source_ids {
        let source = Source {
            id: source_id.clone(),
            path: tempdir.path().join(format!("{}.md", source_id.0)),
            hash: "test-hash".into(),
            status: SourceStatus::Pending,
            parser_used: None,
            last_ingested_at: None,
        };
        store.add_source(&source).unwrap();
        store.remove_source(&source.id).unwrap();
    }
    let pipeline = IngestPipeline::from_parts(
        store,
        HnswIndex::new(),
        ReconcileEmbeddingClient,
        tempdir.path().to_path_buf(),
    );

    let expected_batches = [
        &source_ids[..BATCH_SIZE],
        &source_ids[BATCH_SIZE..],
        &source_ids[..BATCH_SIZE],
        &source_ids[BATCH_SIZE..],
    ];
    let mut report_count = 0;
    for expected_source_ids in expected_batches {
        assert_eq!(
            pipeline
                .reconcile_deletions_up_to(BATCH_SIZE)
                .await
                .unwrap()
                .len(),
            BATCH_SIZE,
        );
        let reports = pipeline.store().list_deletion_reports().unwrap();
        assert_eq!(
            reports[report_count..]
                .iter()
                .map(|report| report.source_id.clone())
                .collect::<Vec<_>>(),
            expected_source_ids,
        );
        report_count = reports.len();
        for source_id in expected_source_ids {
            pipeline
                .store()
                .connection()
                .execute(
                    "UPDATE source_tombstones
                     SET last_reconcile_attempt_ts = 1
                     WHERE source_id = ?1",
                    [&source_id.0],
                )
                .unwrap();
        }
    }
}

#[cfg(feature = "qdrant")]
#[tokio::test]
async fn interrupted_qdrant_compensation_requeues_tombstone_for_reconciliation() {
    let tempdir = tempfile::tempdir().unwrap();
    let store = Store::in_memory().unwrap();
    let source = Source {
        id: SourceId("interrupted-qdrant-compensation".into()),
        path: tempdir.path().join("interrupted-qdrant-compensation.md"),
        hash: "test-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    store.add_source(&source).unwrap();
    store.remove_source(&source.id).unwrap();
    let mut report = DeletionReport::new();
    let mut transaction = store.connection().unchecked_transaction().unwrap();
    store
        .finalize_deletion_outcome_tx(
            &mut transaction,
            &source.id,
            DeletionOutcome::Erased,
            &mut report,
        )
        .unwrap();
    transaction.commit().unwrap();

    // Simulate process exit after an upsert reached Qdrant but before its
    // compensating delete could finish. The stale terminal outcome must be
    // durable-pending so a later process picks the tombstone up.
    store.requeue_qdrant_deletion(&source.id).unwrap();
    let pipeline = IngestPipeline::from_parts(
        store,
        HnswIndex::new(),
        ReconcileEmbeddingClient,
        tempdir.path().to_path_buf(),
    );

    let reports = pipeline.reconcile_deletions_up_to(1).await.unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(
        pipeline
            .store()
            .qdrant_deletion_outcome(&source.id)
            .unwrap(),
        Some(DeletionOutcome::Pending)
    );
}
