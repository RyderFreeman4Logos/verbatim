use super::*;
use crate::deletion::{DeletionOutcome, DeletionProduct, DeletionReport, RetentionPolicy};
use crate::types::{Source, SourceStatus};
use async_trait::async_trait;
use std::sync::{Arc, Barrier};

#[cfg(feature = "qdrant")]
use std::{
    io::{Read, Write},
    net::TcpListener,
    sync::mpsc,
    time::Duration,
};

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

    let mut report = crate::deletion::DeletionReport::new();
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
    let mut report = crate::deletion::DeletionReport::new();
    let mut transaction = store.connection().unchecked_transaction().unwrap();

    store
        .finalize_deletion_outcome_tx(
            &mut transaction,
            &source.id,
            DeletionOutcome::NotFound,
            &mut report,
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
    let mut report = crate::deletion::DeletionReport::new();
    let mut transaction = store.connection().unchecked_transaction().unwrap();

    let error = store
        .finalize_deletion_outcome_tx(
            &mut transaction,
            &source.id,
            DeletionOutcome::Erased,
            &mut report,
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

#[cfg(feature = "qdrant")]
#[tokio::test]
async fn finalize_rechecks_a_legal_hold_placed_during_qdrant_wait() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (delete_started_tx, delete_started_rx) = tokio::sync::oneshot::channel();
    let (release_response_tx, release_response_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut collection_request, _) = listener.accept().unwrap();
        read_hanging_qdrant_request(&mut collection_request);
        let body = r#"{"status":"ok","result":{}}"#;
        write!(
            collection_request,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        )
        .unwrap();

        let (mut deletion_request, _) = listener.accept().unwrap();
        read_hanging_qdrant_request(&mut deletion_request);
        delete_started_tx.send(()).unwrap();
        release_response_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
        let body = r#"{"status":"ok","result":{"status":"acknowledged","operation_id":1}}"#;
        write!(
            deletion_request,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        )
        .unwrap();
    });

    let tempdir = tempfile::tempdir().unwrap();
    let database_path = tempdir.path().join("verbatim.db");
    let source = Source {
        id: SourceId("legal-hold-during-qdrant-wait".into()),
        path: tempdir.path().join("legal-hold-during-qdrant-wait.md"),
        hash: "test-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    let store = Store::new(&database_path).unwrap();
    store.add_source(&source).unwrap();
    let qdrant = crate::index::qdrant::QdrantClient::new(crate::config::QdrantConfig {
        enabled: true,
        url: format!("http://{address}"),
        collection: "verbatim".into(),
        prefer_for_search: false,
        timeout_seconds: 5,
    });
    let mut pipeline = IngestPipeline::from_parts(
        store,
        HnswIndex::new(),
        DeletionEmbeddingClient,
        tempdir.path().to_path_buf(),
    )
    .with_qdrant_client(qdrant);
    let hold_store = Store::new(&database_path).unwrap();

    let mut deletion = Box::pin(pipeline.remove_source(&source.id));
    tokio::select! {
        result = &mut deletion => panic!("deletion completed before Qdrant wait: {result:?}"),
        result = delete_started_rx => result.unwrap(),
    }
    hold_store.place_legal_hold(&source.id).unwrap();
    release_response_tx.send(()).unwrap();
    let report = deletion.await.unwrap();
    server.join().unwrap();

    assert_eq!(
        report.status_for(DeletionProduct::Backups),
        Some(DeletionOutcome::Held),
    );
    let latest = hold_store
        .latest_deletion_report(&source.id)
        .unwrap()
        .unwrap();
    assert_eq!(latest.retention_policy, RetentionPolicy::LegalHold);
    assert_eq!(
        latest.report.status_for(DeletionProduct::Backups),
        Some(DeletionOutcome::Held),
    );
}

#[cfg(feature = "qdrant")]
#[tokio::test]
async fn bounded_reconcile_stops_after_one_hanging_qdrant_attempt() {
    const BATCH_SIZE: usize = 1;
    const TOMBSTONE_COUNT: usize = BATCH_SIZE + 2;

    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (delete_started_tx, delete_started_rx) = mpsc::channel();
    let (release_response_tx, release_response_rx) = mpsc::channel();
    let server = std::thread::spawn(move || {
        let (mut collection_request, _) = listener.accept().unwrap();
        read_hanging_qdrant_request(&mut collection_request);
        let body = r#"{"status":"ok","result":{}}"#;
        write!(
            collection_request,
            "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len(),
        )
        .unwrap();

        let (mut deletion_request, _) = listener.accept().unwrap();
        read_hanging_qdrant_request(&mut deletion_request);
        delete_started_tx.send(()).unwrap();
        release_response_rx
            .recv_timeout(Duration::from_secs(5))
            .unwrap();
    });

    let tempdir = tempfile::tempdir().unwrap();
    let store = Store::in_memory().unwrap();
    for index in 0..TOMBSTONE_COUNT {
        let source = Source {
            id: SourceId(format!("bounded-hanging-qdrant-{index:02}")),
            path: tempdir
                .path()
                .join(format!("bounded-hanging-qdrant-{index:02}.md")),
            hash: "test-hash".into(),
            status: SourceStatus::Pending,
            parser_used: None,
            last_ingested_at: None,
        };
        store.add_source(&source).unwrap();
        store.remove_source(&source.id).unwrap();
    }
    let qdrant = crate::index::qdrant::QdrantClient::new(crate::config::QdrantConfig {
        enabled: true,
        url: format!("http://{address}"),
        collection: "verbatim".into(),
        prefer_for_search: false,
        timeout_seconds: 1,
    });
    let pipeline = IngestPipeline::from_parts(
        store,
        HnswIndex::new(),
        DeletionEmbeddingClient,
        tempdir.path().to_path_buf(),
    )
    .with_qdrant_client(qdrant);

    let reports = pipeline
        .reconcile_deletions_up_to(BATCH_SIZE)
        .await
        .unwrap();

    assert_eq!(delete_started_rx.try_recv(), Ok(()));
    assert_eq!(reports.len(), BATCH_SIZE);
    assert_eq!(
        reports[0].status_for(DeletionProduct::Qdrant),
        Some(DeletionOutcome::Pending),
    );
    assert_eq!(
        pipeline
            .store()
            .pending_qdrant_deletion_source_ids()
            .unwrap()
            .len(),
        TOMBSTONE_COUNT,
    );
    assert_eq!(
        pipeline.store().list_deletion_reports().unwrap().len(),
        BATCH_SIZE,
    );

    release_response_tx.send(()).unwrap();
    server.join().unwrap();
}

#[cfg(feature = "qdrant")]
fn read_hanging_qdrant_request(stream: &mut std::net::TcpStream) {
    let mut request = Vec::new();
    let mut buffer = [0_u8; 1024];
    loop {
        let read = stream.read(&mut buffer).unwrap();
        assert_ne!(read, 0, "qdrant client closed request before sending it");
        request.extend_from_slice(&buffer[..read]);
        let Some(header_end) = request.windows(4).position(|window| window == b"\r\n\r\n") else {
            continue;
        };
        let headers = String::from_utf8_lossy(&request[..header_end]);
        let content_length = headers
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        if request.len() >= header_end + 4 + content_length {
            return;
        }
    }
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

#[tokio::test]
async fn reconcile_refreshes_expired_backups_after_qdrant_is_terminal() {
    let tempdir = tempfile::tempdir().unwrap();
    let store = Store::in_memory().unwrap();
    let source = Source {
        id: SourceId("terminal-qdrant-expired-backups".into()),
        path: tempdir.path().join("expired-backups.md"),
        hash: "test-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    store.add_source(&source).unwrap();
    let retention_policy = RetentionPolicy::UntilBackupExpiry(0);
    store
        .remove_source_with_retention(&source.id, retention_policy)
        .unwrap();
    store
        .connection()
        .execute(
            "UPDATE source_tombstones SET qdrant_outcome = 'erased' WHERE source_id = ?1",
            [&source.id.0],
        )
        .unwrap();
    // Simulate a receipt written by an older process before finalization re-read
    // the tombstone's already-expired retention policy.
    let mut stale_report = DeletionReport::new();
    stale_report.set(DeletionProduct::Qdrant, DeletionOutcome::Erased);
    stale_report.set(DeletionProduct::Backups, DeletionOutcome::Pending);
    store
        .persist_deletion_report(&source.id, retention_policy, &stale_report)
        .unwrap();
    assert!(store
        .pending_qdrant_deletion_source_ids()
        .unwrap()
        .is_empty());

    let pipeline = IngestPipeline::from_parts(
        store,
        HnswIndex::new(),
        DeletionEmbeddingClient,
        tempdir.path().to_path_buf(),
    );
    let reports = pipeline.reconcile_deletions().await.unwrap();

    assert_eq!(reports.len(), 1);
    assert_eq!(
        reports[0].status_for(DeletionProduct::Qdrant),
        Some(DeletionOutcome::Erased),
    );
    assert_eq!(
        reports[0].status_for(DeletionProduct::Backups),
        Some(DeletionOutcome::Erased),
    );
    let latest = pipeline
        .store()
        .latest_deletion_report(&source.id)
        .unwrap()
        .unwrap();
    assert_eq!(latest.report, reports[0]);
}

#[test]
fn retention_and_legal_hold_changes_refresh_the_latest_backup_report() {
    let store = Store::in_memory().unwrap();
    let source = Source {
        id: SourceId("refresh-retention-report".into()),
        path: std::path::PathBuf::from("refresh-retention-report.md"),
        hash: "test-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    store.add_source(&source).unwrap();
    let retention_policy = RetentionPolicy::UntilBackupExpiry(
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_secs()
            .saturating_add(60),
    );
    store
        .remove_source_with_retention(&source.id, retention_policy)
        .unwrap();
    let mut report = DeletionReport::new();
    report.set(DeletionProduct::Qdrant, DeletionOutcome::Erased);
    report.set(DeletionProduct::Backups, DeletionOutcome::Pending);
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

    store.place_legal_hold(&source.id).unwrap();
    let held = store.latest_deletion_report(&source.id).unwrap().unwrap();
    assert_eq!(held.retention_policy, RetentionPolicy::LegalHold);
    assert_eq!(
        held.report.status_for(DeletionProduct::Backups),
        Some(DeletionOutcome::Held),
    );

    store.release_legal_hold(&source.id).unwrap();
    let released = store.latest_deletion_report(&source.id).unwrap().unwrap();
    assert_eq!(released.retention_policy, retention_policy);
    assert_eq!(
        released.report.status_for(DeletionProduct::Backups),
        Some(DeletionOutcome::Pending),
    );

    store
        .set_retention_policy(&source.id, RetentionPolicy::Immediate)
        .unwrap();
    let immediate = store.latest_deletion_report(&source.id).unwrap().unwrap();
    assert_eq!(immediate.retention_policy, RetentionPolicy::Immediate);
    assert_eq!(
        immediate.report.status_for(DeletionProduct::Backups),
        Some(DeletionOutcome::Erased),
    );
}
