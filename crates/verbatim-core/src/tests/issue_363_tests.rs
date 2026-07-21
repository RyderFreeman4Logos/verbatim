use super::*;
use crate::deletion::{DeletionOutcome, DeletionProduct, RetentionPolicy};
use crate::types::{Source, SourceStatus};
use async_trait::async_trait;

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
    assert_eq!(
        report.status_for(DeletionProduct::Hnsw),
        Some(DeletionOutcome::Erased),
    );
    for product in [
        DeletionProduct::Chunks,
        DeletionProduct::Vectors,
        DeletionProduct::Graph,
        DeletionProduct::Images,
        DeletionProduct::Caches,
        DeletionProduct::Backups,
    ] {
        assert_eq!(report.status_for(product), Some(DeletionOutcome::Erased));
    }
    assert_eq!(
        report.status_for(DeletionProduct::Qdrant),
        Some(DeletionOutcome::NotFound),
    );
    assert!(pipeline.hnsw().search(&[1.0, 0.0], 1).is_empty());
    assert!(pipeline.store().is_tombstoned(&source_id).unwrap());
    assert!(pipeline
        .add_source(&path)
        .unwrap_err()
        .to_string()
        .contains("tombstoned"));
    assert!(!format!("{report:?}").contains(restricted_content));
}

#[test]
fn persisted_tombstone_retention_honors_backup_expiry_and_legal_hold() {
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

    store
        .set_qdrant_deletion_outcome(&source.id, DeletionOutcome::Erased)
        .unwrap();
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
