use super::*;
use crate::deletion::{DeletionOutcome, DeletionProduct};
use crate::types::{Source, SourceStatus};
use async_trait::async_trait;

struct NoQdrantEmbeddingClient;

#[async_trait]
impl EmbeddingClient for NoQdrantEmbeddingClient {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }

    fn dimension(&self) -> usize {
        2
    }
}

#[tokio::test]
async fn no_qdrant_reconcile_terminalizes_preexisting_pending_tombstone_once() {
    let tempdir = tempfile::tempdir().unwrap();
    let database_path = tempdir.path().join("verbatim.db");
    let source = Source {
        id: SourceId("no-qdrant-preexisting-pending".into()),
        path: tempdir.path().join("no-qdrant-preexisting-pending.md"),
        hash: "test-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    let store = Store::new(&database_path).unwrap();
    store.add_source(&source).unwrap();
    store.remove_source(&source.id).unwrap();
    assert_eq!(
        store.qdrant_deletion_outcome(&source.id).unwrap(),
        Some(DeletionOutcome::Pending),
    );
    let initial_report_count = store.list_deletion_reports().unwrap().len();
    drop(store);

    let pipeline = IngestPipeline::from_parts(
        Store::new(&database_path).unwrap(),
        HnswIndex::new(),
        NoQdrantEmbeddingClient,
        tempdir.path().to_path_buf(),
    );

    let first_reconcile = pipeline.reconcile_deletions().await.unwrap();

    assert_eq!(first_reconcile.len(), 1);
    assert_eq!(
        first_reconcile[0].status_for(DeletionProduct::Qdrant),
        Some(DeletionOutcome::NotFound),
    );
    assert_eq!(
        pipeline
            .store()
            .qdrant_deletion_outcome(&source.id)
            .unwrap(),
        Some(DeletionOutcome::NotFound),
    );
    let report_count_after_first_reconcile =
        pipeline.store().list_deletion_reports().unwrap().len();
    assert_eq!(report_count_after_first_reconcile, initial_report_count + 1);

    assert!(pipeline.reconcile_deletions().await.unwrap().is_empty());
    assert_eq!(
        pipeline.store().list_deletion_reports().unwrap().len(),
        report_count_after_first_reconcile,
    );
}
