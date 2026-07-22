use super::*;
use crate::deletion::{DeletionOutcome, DeletionProduct, RetentionPolicy};
use crate::types::{Source, SourceStatus};
use async_trait::async_trait;
use std::sync::{Arc, Barrier};

struct DeletionEmbeddingClient;

#[async_trait]
impl EmbeddingClient for DeletionEmbeddingClient {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }

    fn dimension(&self) -> usize {
        2
    }
}

#[tokio::test]
async fn source_erasure_removes_local_derivatives_tombstones_and_blocks_reingest() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("restricted-source.md");
    let restricted_content = "restricted source body must never appear in a deletion report";
    fs::write(&path, restricted_content).unwrap();
    let store = Store::in_memory().unwrap();
    let mut pipeline = IngestPipeline::from_parts(
        store,
        HnswIndex::new(),
        DeletionEmbeddingClient,
        tempdir.path().to_path_buf(),
    );
    let source_id = pipeline.add_source(&path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();

    let report = pipeline.remove_source(&source_id).await.unwrap();

    assert!(pipeline.store().get_source(&source_id).unwrap().is_none());
    assert!(pipeline
        .store()
        .list_evidence_by_source(&source_id)
        .unwrap()
        .is_empty());
    assert!(pipeline
        .store()
        .list_chunks_by_source(&source_id)
        .unwrap()
        .is_empty());
    assert_eq!(
        pipeline
            .store()
            .count_vector_documents_for_profile(
                &EmbeddingProfileId::default_profile(),
                Some(&source_id),
            )
            .unwrap(),
        0
    );
    assert_eq!(
        report.status_for(DeletionProduct::SqliteAuthoritative),
        Some(DeletionOutcome::Erased),
    );
    for product in [
        DeletionProduct::Chunks,
        DeletionProduct::Vectors,
        DeletionProduct::Graph,
        DeletionProduct::Images,
    ] {
        assert_eq!(report.status_for(product), Some(DeletionOutcome::Erased));
    }
    for product in [DeletionProduct::Hnsw, DeletionProduct::Caches] {
        assert_eq!(report.status_for(product), Some(DeletionOutcome::Pending));
    }
    assert_eq!(
        report.status_for(DeletionProduct::Backups),
        Some(DeletionOutcome::Erased),
    );
    assert_eq!(
        report.status_for(DeletionProduct::Qdrant),
        Some(DeletionOutcome::Pending),
    );
    assert!(pipeline.hnsw().search(&[1.0, 0.0], 1).is_empty());
    assert!(pipeline.store().is_tombstoned(&source_id).unwrap());
    assert!(pipeline
        .add_source(&path)
        .unwrap_err()
        .to_string()
        .contains("tombstoned"));
    let persisted_reports = pipeline.store().list_deletion_reports().unwrap();
    assert_eq!(persisted_reports.len(), 1);
    assert_eq!(persisted_reports[0].source_id, source_id);
    assert_eq!(persisted_reports[0].report, report);
    assert!(!format!("{report:?}").contains(restricted_content));
}

#[test]
fn persisted_tombstone_retention_keeps_backups_pending_until_cleaned_or_held() {
    let store = Store::in_memory().unwrap();
    let source = Source {
        id: SourceId("src-retention".into()),
        path: std::path::PathBuf::from("retained-source.md"),
        hash: "test-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    store.add_source(&source).unwrap();
    store
        .remove_source_with_retention(&source.id, RetentionPolicy::UntilBackupExpiry(20))
        .unwrap();

    assert_eq!(
        store.retention_policy(&source.id).unwrap(),
        Some(RetentionPolicy::UntilBackupExpiry(20)),
    );
    assert_eq!(
        store.backup_deletion_outcome_at(&source.id, 19).unwrap(),
        DeletionOutcome::Pending,
    );
    assert_eq!(
        store.backup_deletion_outcome_at(&source.id, 20).unwrap(),
        DeletionOutcome::Erased,
    );
    assert_eq!(
        store.pending_qdrant_deletion_source_ids().unwrap(),
        vec![source.id.clone()],
    );

    store.place_legal_hold(&source.id).unwrap();
    assert_eq!(
        store.backup_deletion_outcome_at(&source.id, 20).unwrap(),
        DeletionOutcome::Held,
    );
    store.release_legal_hold(&source.id).unwrap();
    assert_eq!(
        store.backup_deletion_outcome_at(&source.id, 20).unwrap(),
        DeletionOutcome::Erased,
    );

    let report = crate::deletion::DeletionReport::new();
    let mut transaction = store.connection().unchecked_transaction().unwrap();
    store
        .finalize_deletion_outcome_tx(
            &mut transaction,
            &source.id,
            DeletionOutcome::Erased,
            RetentionPolicy::UntilBackupExpiry(20),
            &report,
        )
        .unwrap();
    transaction.commit().unwrap();
    assert!(store
        .pending_qdrant_deletion_source_ids()
        .unwrap()
        .is_empty());
    assert!(store
        .add_source(&source)
        .unwrap_err()
        .to_string()
        .contains("tombstoned"));
}

#[test]
fn replace_source_contents_cannot_resurrect_tombstoned_source() {
    let store = Store::in_memory().unwrap();
    let source = Source {
        id: SourceId("replace-tombstoned-source".into()),
        path: std::path::PathBuf::from("replace-tombstoned-source.md"),
        hash: "test-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    let profile = EmbeddingProfileId::default_profile();
    store.add_source(&source).unwrap();
    store.remove_source(&source.id).unwrap();

    let error = store
        .replace_source_contents(SourceContentsReplacement {
            source: &source,
            evidence: &[],
            chunks: &[],
            embedding_profile_id: &profile,
            vectors: &[],
            links: &[],
            evidence_spans: &[],
            image_artifacts: &[],
            graph_nodes: &[],
            graph_edges: &[],
        })
        .unwrap_err();

    assert!(error.to_string().contains("source is tombstoned"));
    assert!(store.get_source(&source.id).unwrap().is_none());
}

#[test]
fn finalizing_deletion_outcome_persists_report_after_deletion() {
    let store = Store::in_memory().unwrap();
    let source = Source {
        id: SourceId("finalize-deletion-outcome".into()),
        path: std::path::PathBuf::from("finalize-deletion-outcome.md"),
        hash: "test-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    store.add_source(&source).unwrap();
    store.remove_source(&source.id).unwrap();
    let report = crate::deletion::DeletionReport::new();
    let mut transaction = store.connection().unchecked_transaction().unwrap();

    store
        .finalize_deletion_outcome_tx(
            &mut transaction,
            &source.id,
            DeletionOutcome::NotFound,
            RetentionPolicy::Immediate,
            &report,
        )
        .unwrap();
    transaction.commit().unwrap();

    let persisted_reports = store.list_deletion_reports().unwrap();
    assert_eq!(persisted_reports.len(), 1);
    assert_eq!(persisted_reports[0].source_id, source.id);
    assert_eq!(persisted_reports[0].report, report);
    assert!(store
        .pending_qdrant_deletion_source_ids()
        .unwrap()
        .is_empty());
}

#[test]
fn failed_deletion_report_insert_rolls_back_qdrant_outcome() {
    let store = Store::in_memory().unwrap();
    let source = Source {
        id: SourceId("failed-deletion-report".into()),
        path: std::path::PathBuf::from("failed-deletion-report.md"),
        hash: "test-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    store.add_source(&source).unwrap();
    store.remove_source(&source.id).unwrap();
    store
        .connection()
        .execute_batch(
            "CREATE TRIGGER fail_deletion_report_insert
             BEFORE INSERT ON deletion_reports
             BEGIN
                 SELECT RAISE(ABORT, 'forced report insert failure');
             END;",
        )
        .unwrap();
    let report = crate::deletion::DeletionReport::new();
    let mut transaction = store.connection().unchecked_transaction().unwrap();

    let error = store
        .finalize_deletion_outcome_tx(
            &mut transaction,
            &source.id,
            DeletionOutcome::Erased,
            RetentionPolicy::Immediate,
            &report,
        )
        .unwrap_err();
    assert!(error.to_string().contains("forced report insert failure"));
    drop(transaction);

    assert_eq!(
        store.pending_qdrant_deletion_source_ids().unwrap(),
        vec![source.id],
    );
    assert!(store.list_deletion_reports().unwrap().is_empty());
}

#[tokio::test]
async fn missing_source_housekeeping_does_not_tombstone_the_source_id() {
    let tempdir = tempfile::tempdir().unwrap();
    let path = tempdir.path().join("missing-source.md");
    fs::write(&path, "temporary source").unwrap();
    let store = Store::in_memory().unwrap();
    let mut pipeline = IngestPipeline::from_parts(
        store,
        HnswIndex::new(),
        DeletionEmbeddingClient,
        tempdir.path().to_path_buf(),
    );
    let source_id = pipeline.add_source(&path).unwrap();

    fs::remove_file(&path).unwrap();
    assert_eq!(
        pipeline
            .remove_missing_sources_for_all_source_ingest(None)
            .await
            .unwrap(),
        vec![source_id.clone()],
    );
    assert!(!pipeline.store().is_tombstoned(&source_id).unwrap());

    fs::write(&path, "restored source").unwrap();
    assert_eq!(pipeline.add_source(&path).unwrap(), source_id);
}

#[test]
fn concurrent_erasure_cannot_reintroduce_a_tombstoned_source() {
    let tempdir = tempfile::tempdir().unwrap();
    let database_path = tempdir.path().join("verbatim.db");
    let source = Source {
        id: SourceId("concurrent-source".into()),
        path: std::path::PathBuf::from("concurrent-source.md"),
        hash: "test-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    let store = Store::new(&database_path).unwrap();
    store.add_source(&source).unwrap();
    drop(store);

    let barrier = Arc::new(Barrier::new(2));
    let eraser_barrier = Arc::clone(&barrier);
    let eraser_path = database_path.clone();
    let eraser_source = source.clone();
    let eraser = std::thread::spawn(move || {
        let store = Store::new(&eraser_path).unwrap();
        eraser_barrier.wait();
        store.remove_source(&eraser_source.id).unwrap();
    });
    let adder_barrier = Arc::clone(&barrier);
    let adder_path = database_path.clone();
    let adder_source = source.clone();
    let adder = std::thread::spawn(move || {
        let store = Store::new(&adder_path).unwrap();
        adder_barrier.wait();
        store.add_source(&adder_source)
    });

    eraser.join().unwrap();
    let _ = adder.join().unwrap();
    let store = Store::new(&database_path).unwrap();
    assert!(store.is_tombstoned(&source.id).unwrap());
    assert!(store.get_source(&source.id).unwrap().is_none());
}

#[tokio::test]
async fn restart_reconciles_pending_deletion_and_persists_the_retry_report() {
    let tempdir = tempfile::tempdir().unwrap();
    let database_path = tempdir.path().join("verbatim.db");
    let source = Source {
        id: SourceId("restart-source".into()),
        path: std::path::PathBuf::from("restart-source.md"),
        hash: "test-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    let store = Store::new(&database_path).unwrap();
    store.add_source(&source).unwrap();
    store.remove_source(&source.id).unwrap();
    drop(store);

    let pipeline = IngestPipeline::from_parts(
        Store::new(&database_path).unwrap(),
        HnswIndex::new(),
        DeletionEmbeddingClient,
        tempdir.path().to_path_buf(),
    );
    let reports = pipeline.reconcile_deletions().await.unwrap();

    assert_eq!(reports.len(), 1);
    assert_eq!(
        reports[0].status_for(DeletionProduct::Qdrant),
        Some(DeletionOutcome::Pending),
    );
    let persisted_reports = pipeline.store().list_deletion_reports().unwrap();
    assert_eq!(persisted_reports.len(), 1);
    assert_eq!(persisted_reports[0].source_id, source.id);
    assert_eq!(persisted_reports[0].report, reports[0]);
}
