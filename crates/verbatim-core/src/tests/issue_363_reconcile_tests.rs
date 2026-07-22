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

#[tokio::test]
async fn bounded_reconcile_round_robins_pending_tombstones_across_successive_batches() {
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

    assert_eq!(
        pipeline
            .reconcile_deletions_up_to(BATCH_SIZE)
            .await
            .unwrap()
            .len(),
        BATCH_SIZE,
    );
    let first_batch_source_ids = pipeline
        .store()
        .list_deletion_reports()
        .unwrap()
        .into_iter()
        .map(|report| report.source_id)
        .collect::<Vec<_>>();
    assert_eq!(first_batch_source_ids, source_ids[..BATCH_SIZE]);

    assert_eq!(
        pipeline
            .reconcile_deletions_up_to(BATCH_SIZE)
            .await
            .unwrap()
            .len(),
        BATCH_SIZE,
    );
    let all_batch_source_ids = pipeline
        .store()
        .list_deletion_reports()
        .unwrap()
        .into_iter()
        .map(|report| report.source_id)
        .collect::<Vec<_>>();
    assert_eq!(all_batch_source_ids, source_ids);
}
