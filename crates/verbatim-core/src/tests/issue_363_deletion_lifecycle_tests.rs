use super::*;
use crate::deletion::{DeletionOutcome, DeletionProduct, RetentionPolicy};
use crate::types::{Source, SourceStatus};
use async_trait::async_trait;

struct DeletionLifecycleEmbeddingClient;

#[async_trait]
impl EmbeddingClient for DeletionLifecycleEmbeddingClient {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }

    fn dimension(&self) -> usize {
        2
    }
}

#[test]
fn local_erasure_commits_an_initial_pending_receipt_with_its_tombstone() {
    let store = Store::in_memory().unwrap();
    let source = Source {
        id: SourceId("initial-deletion-receipt".into()),
        path: std::path::PathBuf::from("initial-deletion-receipt.md"),
        hash: "test-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    store.add_source(&source).unwrap();

    store.remove_source(&source.id).unwrap();

    assert!(store.is_tombstoned(&source.id).unwrap());
    let reports = store.list_deletion_reports().unwrap();
    assert_eq!(reports.len(), 1);
    assert_eq!(reports[0].source_id, source.id);
    assert_eq!(
        reports[0].report.status_for(DeletionProduct::Qdrant),
        Some(DeletionOutcome::Pending),
    );
    assert_eq!(
        reports[0].report.status_for(DeletionProduct::Images),
        Some(DeletionOutcome::Pending),
    );
}

#[tokio::test]
async fn reconciliation_retries_a_persisted_pending_image_cleanup() {
    let tempdir = tempfile::tempdir().unwrap();
    let store = Store::in_memory().unwrap();
    let mut pipeline = IngestPipeline::from_parts(
        store,
        HnswIndex::new(),
        DeletionLifecycleEmbeddingClient,
        tempdir.path().to_path_buf(),
    );
    let source = Source {
        id: SourceId("retry-pending-image-cleanup".into()),
        path: tempdir.path().join("retry-pending-image-cleanup.md"),
        hash: "test-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    pipeline.store().add_source(&source).unwrap();
    let blocking_path = tempdir
        .path()
        .join("image-artifacts")
        .join("retry-pending-image-cleanup");
    std::fs::create_dir_all(blocking_path.parent().unwrap()).unwrap();
    std::fs::write(&blocking_path, b"not a directory").unwrap();

    let first_report = pipeline.remove_source(&source.id).await.unwrap();
    assert_eq!(
        first_report.status_for(DeletionProduct::Images),
        Some(DeletionOutcome::Pending),
    );

    std::fs::remove_dir_all(tempdir.path().join("image-artifacts")).unwrap();
    let retry_reports = pipeline.reconcile_deletions_up_to(1).await.unwrap();

    assert_eq!(retry_reports.len(), 1);
    assert_eq!(
        retry_reports[0].status_for(DeletionProduct::Images),
        Some(DeletionOutcome::Erased),
    );
    let latest = pipeline
        .store()
        .latest_deletion_report(&source.id)
        .unwrap()
        .unwrap();
    assert_eq!(
        latest.report.status_for(DeletionProduct::Images),
        Some(DeletionOutcome::Erased),
    );
}

#[tokio::test]
async fn disabled_qdrant_tombstone_is_not_selected_on_repeated_scheduler_ticks() {
    let tempdir = tempfile::tempdir().unwrap();
    let source_path = tempdir.path().join("disabled-qdrant-source.md");
    std::fs::write(&source_path, "Qdrant is disabled for this deletion").unwrap();
    let store = Store::in_memory().unwrap();
    let mut pipeline = IngestPipeline::from_parts(
        store,
        HnswIndex::new(),
        DeletionLifecycleEmbeddingClient,
        tempdir.path().to_path_buf(),
    );
    let source_id = pipeline.add_source(&source_path).unwrap();

    let report = pipeline.remove_source(&source_id).await.unwrap();

    assert_eq!(
        report.status_for(DeletionProduct::Qdrant),
        Some(DeletionOutcome::NotFound),
    );
    assert_eq!(
        pipeline
            .store()
            .qdrant_deletion_outcome(&source_id)
            .unwrap(),
        Some(DeletionOutcome::NotFound),
    );
    let report_count = pipeline.store().list_deletion_reports().unwrap().len();
    for _ in 0..3 {
        assert!(pipeline
            .reconcile_deletions_up_to(16)
            .await
            .unwrap()
            .is_empty());
    }
    assert_eq!(
        pipeline.store().list_deletion_reports().unwrap().len(),
        report_count
    );
}

#[cfg(feature = "qdrant")]
#[tokio::test]
async fn configured_qdrant_failure_remains_pending_and_retryable() {
    let tempdir = tempfile::tempdir().unwrap();
    let source_path = tempdir.path().join("retryable-qdrant-source.md");
    std::fs::write(&source_path, "Qdrant is configured but unavailable").unwrap();
    let qdrant = crate::index::qdrant::QdrantClient::new(crate::config::QdrantConfig {
        enabled: true,
        url: "http://127.0.0.1:9".into(),
        collection: "verbatim".into(),
        prefer_for_search: false,
        timeout_seconds: 1,
    });
    let store = Store::in_memory().unwrap();
    let mut pipeline = IngestPipeline::from_parts(
        store,
        HnswIndex::new(),
        DeletionLifecycleEmbeddingClient,
        tempdir.path().to_path_buf(),
    )
    .with_qdrant_client(qdrant);
    let source_id = pipeline.add_source(&source_path).unwrap();

    let initial = pipeline.remove_source(&source_id).await.unwrap();

    assert_eq!(
        initial.status_for(DeletionProduct::Qdrant),
        Some(DeletionOutcome::Pending),
    );
    let report_count = pipeline.store().list_deletion_reports().unwrap().len();
    let retry = pipeline.reconcile_deletions_up_to(1).await.unwrap();
    assert_eq!(retry.len(), 1);
    assert_eq!(
        retry[0].status_for(DeletionProduct::Qdrant),
        Some(DeletionOutcome::Pending),
    );
    assert_eq!(
        pipeline.store().list_deletion_reports().unwrap().len(),
        report_count + 1
    );
}

#[tokio::test]
async fn reconciliation_selects_only_actionable_tombstones() {
    let tempdir = tempfile::tempdir().unwrap();
    let store = Store::in_memory().unwrap();
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_secs();
    let policies = [
        ("terminal", RetentionPolicy::Immediate),
        ("held", RetentionPolicy::LegalHold),
        (
            "future-backup",
            RetentionPolicy::UntilBackupExpiry(now.saturating_add(86_400)),
        ),
    ];

    for (id, policy) in policies {
        let source = Source {
            id: SourceId(id.into()),
            path: tempdir.path().join(format!("{id}.md")),
            hash: format!("{id}-hash"),
            status: SourceStatus::Pending,
            parser_used: None,
            last_ingested_at: None,
        };
        store.add_source(&source).unwrap();
        store
            .remove_source_with_retention(&source.id, policy)
            .unwrap();
        let mut report = store
            .latest_deletion_report(&source.id)
            .unwrap()
            .unwrap()
            .report;
        report.set(DeletionProduct::Qdrant, DeletionOutcome::Erased);
        report.set(DeletionProduct::Images, DeletionOutcome::Erased);
        let mut transaction = store.connection().unchecked_transaction().unwrap();
        store
            .finalize_deletion_outcomes_tx(
                &mut transaction,
                &source.id,
                DeletionOutcome::Erased,
                DeletionOutcome::Erased,
                &mut report,
            )
            .unwrap();
        transaction.commit().unwrap();
    }

    assert!(store
        .reconciliation_deletion_source_ids()
        .unwrap()
        .is_empty());
    let report_count = store.list_deletion_reports().unwrap().len();
    let pipeline = IngestPipeline::from_parts(
        store,
        HnswIndex::new(),
        DeletionLifecycleEmbeddingClient,
        tempdir.path().to_path_buf(),
    );
    assert!(pipeline
        .reconcile_deletions_up_to(16)
        .await
        .unwrap()
        .is_empty());
    assert_eq!(
        pipeline.store().list_deletion_reports().unwrap().len(),
        report_count
    );
}

#[test]
fn qdrant_enabled_runtime_config_requires_compiled_support() {
    assert!(validate_qdrant_runtime_support(true, false).is_err());
    assert!(validate_qdrant_runtime_support(false, false).is_ok());
    assert!(validate_qdrant_runtime_support(true, true).is_ok());
}

#[test]
fn tombstone_reconcile_index_migrates_once_to_the_actionable_predicate() {
    let tempdir = tempfile::tempdir().unwrap();
    let db_path = tempdir.path().join("verbatim.db");
    drop(Store::new(&db_path).unwrap());

    let connection = rusqlite::Connection::open(&db_path).unwrap();
    connection
        .execute_batch(
            "DROP INDEX source_tombstones_reconcile_attempt_idx;
             CREATE INDEX source_tombstones_reconcile_attempt_idx
                ON source_tombstones(last_reconcile_attempt_seq, source_id)
                WHERE qdrant_outcome = 'pending'
                   OR legal_hold = 1
                   OR backup_expiry_at IS NOT NULL;",
        )
        .unwrap();
    drop(connection);

    drop(Store::new(&db_path).unwrap());
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    let index_sql: String = connection
        .query_row(
            "SELECT sql FROM sqlite_master
             WHERE type = 'index' AND name = 'source_tombstones_reconcile_attempt_idx'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert!(index_sql.contains("images_outcome = 'pending'"));
    assert!(index_sql.contains("legal_hold = 0"));
    let schema_version_before: i64 = connection
        .query_row("PRAGMA schema_version", [], |row| row.get(0))
        .unwrap();
    drop(connection);

    drop(Store::new(&db_path).unwrap());
    let connection = rusqlite::Connection::open(&db_path).unwrap();
    let schema_version_after: i64 = connection
        .query_row("PRAGMA schema_version", [], |row| row.get(0))
        .unwrap();
    assert_eq!(schema_version_after, schema_version_before);
}
