use super::*;
#[cfg(feature = "qdrant")]
use crate::deletion::{DeletionOutcome, DeletionReport};
use crate::types::{Source, SourceStatus};
use crate::vision_caption::{CaptionAttempt, VISION_CAPTION_PROMPT_VERSION};
#[cfg(feature = "qdrant")]
use crate::{config::QdrantConfig, index::qdrant::QdrantClient};
use async_trait::async_trait;
#[cfg(feature = "qdrant")]
use std::io::{Read, Write};
#[cfg(feature = "qdrant")]
use std::net::{TcpListener, TcpStream};
use std::sync::{Arc, Mutex};
#[cfg(feature = "qdrant")]
use std::{sync::mpsc, thread, time::Duration};

#[cfg(feature = "qdrant")]
struct StaticEmbeddingClient;

#[cfg(feature = "qdrant")]
#[async_trait]
impl EmbeddingClient for StaticEmbeddingClient {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }

    fn dimension(&self) -> usize {
        2
    }
}

struct PausingEmbeddingClient {
    started: Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    release: Arc<tokio::sync::Notify>,
}

#[async_trait]
impl EmbeddingClient for PausingEmbeddingClient {
    async fn embed(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        self.started
            .lock()
            .unwrap()
            .take()
            .unwrap()
            .send(())
            .unwrap();
        self.release.notified().await;
        Ok(texts.iter().map(|_| vec![1.0, 0.0]).collect())
    }

    fn dimension(&self) -> usize {
        2
    }
}

#[tokio::test]
async fn tombstone_fence_prevents_inflight_ingest_from_recreating_embedding_cache() {
    let tempdir = tempfile::tempdir().unwrap();
    let database_path = tempdir.path().join("verbatim.db");
    let source_path = tempdir.path().join("inflight-cache-source.md");
    fs::write(&source_path, "in-flight source body").unwrap();
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let release = Arc::new(tokio::sync::Notify::new());
    let mut pipeline = IngestPipeline::from_parts(
        Store::new(&database_path).unwrap(),
        HnswIndex::new(),
        PausingEmbeddingClient {
            started: Mutex::new(Some(started_tx)),
            release: Arc::clone(&release),
        },
        tempdir.path().to_path_buf(),
    );
    let source_id = pipeline.add_source(&source_path).unwrap();

    let mut ingest = Box::pin(pipeline.ingest_source(&source_id));
    tokio::select! {
        result = &mut ingest => panic!("ingest completed before embedding pause: {result:?}"),
        result = started_rx => result.unwrap(),
    }

    Store::new(&database_path)
        .unwrap()
        .remove_source(&source_id)
        .unwrap();
    release.notify_one();
    assert!(ingest.await.is_err());

    let store = Store::new(&database_path).unwrap();
    let cache_rows: i64 = store
        .connection()
        .query_row("SELECT COUNT(*) FROM embedding_cache", [], |row| row.get(0))
        .unwrap();
    assert!(store.is_tombstoned(&source_id).unwrap());
    assert_eq!(cache_rows, 0);
}

#[test]
fn tombstone_fence_skips_image_caption_cache_write() {
    let store = Store::in_memory().unwrap();
    let source = Source {
        id: SourceId("tombstoned-image-cache-source".into()),
        path: std::path::PathBuf::from("tombstoned-image-cache-source.md"),
        hash: "test-hash".into(),
        status: SourceStatus::Pending,
        parser_used: None,
        last_ingested_at: None,
    };
    store.add_source(&source).unwrap();
    store.remove_source(&source.id).unwrap();

    store
        .upsert_image_caption_attempt_for_live_source(
            &source.id,
            "image-hash",
            "vision-test",
            VISION_CAPTION_PROMPT_VERSION,
            "prompt-hash",
            &CaptionAttempt::skipped("deleted before caption write"),
        )
        .unwrap();

    assert!(store
        .get_image_caption("image-hash", "vision-test", "prompt-hash")
        .unwrap()
        .is_none());
}

#[cfg(feature = "qdrant")]
#[derive(Debug)]
struct TestHttpRequest {
    line: String,
    body: String,
}

#[cfg(feature = "qdrant")]
fn qdrant_test_config(url: String) -> QdrantConfig {
    QdrantConfig {
        enabled: true,
        url,
        collection: "verbatim".into(),
        prefer_for_search: false,
        timeout_seconds: 2,
    }
}

#[cfg(feature = "qdrant")]
fn spawn_pausing_qdrant_upsert_server(
    request_count: usize,
    failed_request_index: Option<usize>,
) -> (
    String,
    tokio::sync::oneshot::Receiver<()>,
    mpsc::Sender<()>,
    mpsc::Sender<()>,
    thread::JoinHandle<Vec<TestHttpRequest>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    listener.set_nonblocking(true).unwrap();
    let address = listener.local_addr().unwrap();
    let (upsert_started_tx, upsert_started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let (stop_tx, stop_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut upsert_started_tx = Some(upsert_started_tx);
        let mut requests = Vec::new();
        while requests.len() < request_count {
            match listener.accept() {
                Ok((mut stream, _)) => {
                    let request_index = requests.len();
                    requests.push(read_http_request(&mut stream));
                    if request_index == 3 {
                        upsert_started_tx.take().unwrap().send(()).unwrap();
                        release_rx.recv().unwrap();
                    }
                    let status = if failed_request_index == Some(request_index) {
                        500
                    } else {
                        200
                    };
                    write_http_response(&mut stream, status, r#"{"status":"ok","result":{}}"#);
                }
                Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                    if stop_rx.try_recv().is_ok() {
                        break;
                    }
                    thread::park_timeout(Duration::from_millis(1));
                }
                Err(error) => panic!("accept qdrant test request: {error}"),
            }
        }
        requests
    });
    (
        format!("http://{address}"),
        upsert_started_rx,
        release_tx,
        stop_tx,
        handle,
    )
}

#[cfg(feature = "qdrant")]
fn read_http_request(stream: &mut TcpStream) -> TestHttpRequest {
    let mut buffer = Vec::new();
    let mut chunk = [0u8; 1024];
    loop {
        let count = stream.read(&mut chunk).unwrap();
        if count == 0 {
            break;
        }
        buffer.extend_from_slice(&chunk[..count]);
        if http_request_complete(&buffer) {
            break;
        }
    }
    let text = String::from_utf8(buffer).unwrap();
    let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
    TestHttpRequest {
        line: head.lines().next().unwrap_or_default().to_owned(),
        body: body.to_owned(),
    }
}

#[cfg(feature = "qdrant")]
fn http_request_complete(buffer: &[u8]) -> bool {
    let text = String::from_utf8_lossy(buffer);
    let Some((head, body)) = text.split_once("\r\n\r\n") else {
        return false;
    };
    let content_length = head
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().ok())
                .flatten()
        })
        .unwrap_or(0);
    body.len() >= content_length
}

#[cfg(feature = "qdrant")]
fn write_http_response(stream: &mut TcpStream, status: u16, body: &str) {
    write!(
        stream,
        "HTTP/1.1 {status} Test\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
        body.len(),
        body
    )
    .unwrap();
    stream.flush().unwrap();
}

#[cfg(feature = "qdrant")]
#[tokio::test]
async fn tombstone_fence_compensates_qdrant_upsert_racing_source_deletion() {
    let tempdir = tempfile::tempdir().unwrap();
    let database_path = tempdir.path().join("verbatim.db");
    let source_path = tempdir.path().join("inflight-qdrant-source.md");
    fs::write(&source_path, "in-flight qdrant source body").unwrap();
    let mut pipeline = IngestPipeline::from_parts(
        Store::new(&database_path).unwrap(),
        HnswIndex::new(),
        StaticEmbeddingClient,
        tempdir.path().to_path_buf(),
    );
    let source_id = pipeline.add_source(&source_path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();

    let (qdrant_url, upsert_started, release, _stop, server) =
        spawn_pausing_qdrant_upsert_server(6, None);
    pipeline = pipeline.with_qdrant_client(QdrantClient::new(qdrant_test_config(qdrant_url)));
    let mut sync = Box::pin(pipeline.sync_qdrant_source(&source_id));
    tokio::select! {
        _ = &mut sync => panic!("qdrant sync completed before the upsert pause"),
        result = upsert_started => result.unwrap(),
    }

    Store::new(&database_path)
        .unwrap()
        .remove_source(&source_id)
        .unwrap();
    release.send(()).unwrap();
    sync.await;

    let requests = server.join().unwrap();
    assert_eq!(
        requests[3].line,
        "PUT /collections/verbatim/points?wait=true HTTP/1.1"
    );
    assert_eq!(
        requests[5].line,
        "POST /collections/verbatim/points/delete?wait=true HTTP/1.1"
    );
    let delete_body: serde_json::Value = serde_json::from_str(&requests[5].body).unwrap();
    assert_eq!(delete_body["filter"]["must"][1]["key"], "source_id");
    assert_eq!(
        delete_body["filter"]["must"][1]["match"]["value"],
        source_id.0
    );
}

#[cfg(feature = "qdrant")]
fn finalize_qdrant_outcome(store: &Store, source_id: &SourceId, outcome: DeletionOutcome) {
    let mut report = DeletionReport::new();
    let mut transaction = store.connection().unchecked_transaction().unwrap();
    store
        .finalize_deletion_outcome_tx(&mut transaction, source_id, outcome, &mut report)
        .unwrap();
    transaction.commit().unwrap();
}

#[cfg(feature = "qdrant")]
#[tokio::test]
async fn tombstone_fence_marks_failed_qdrant_compensation_pending() {
    let tempdir = tempfile::tempdir().unwrap();
    let database_path = tempdir.path().join("verbatim.db");
    let source_path = tempdir.path().join("failed-qdrant-compensation.md");
    fs::write(&source_path, "in-flight qdrant source body").unwrap();
    let mut pipeline = IngestPipeline::from_parts(
        Store::new(&database_path).unwrap(),
        HnswIndex::new(),
        StaticEmbeddingClient,
        tempdir.path().to_path_buf(),
    );
    let source_id = pipeline.add_source(&source_path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();

    let (qdrant_url, upsert_started, release, _stop, server) =
        spawn_pausing_qdrant_upsert_server(6, Some(5));
    pipeline = pipeline.with_qdrant_client(QdrantClient::new(qdrant_test_config(qdrant_url)));
    let mut sync = Box::pin(pipeline.sync_qdrant_source(&source_id));
    tokio::select! {
        _ = &mut sync => panic!("qdrant sync completed before the upsert pause"),
        result = upsert_started => result.unwrap(),
    }

    let store = Store::new(&database_path).unwrap();
    store.remove_source(&source_id).unwrap();
    finalize_qdrant_outcome(&store, &source_id, DeletionOutcome::Erased);
    release.send(()).unwrap();
    sync.await;

    let requests = server.join().unwrap();
    assert_eq!(
        requests[5].line,
        "POST /collections/verbatim/points/delete?wait=true HTTP/1.1"
    );
    assert_eq!(
        store.qdrant_deletion_outcome(&source_id).unwrap(),
        Some(DeletionOutcome::Pending)
    );
}

#[cfg(feature = "qdrant")]
#[tokio::test]
async fn tombstone_fence_process_exit_after_qdrant_upsert_reconciles_pending_tombstone() {
    let tempdir = tempfile::tempdir().unwrap();
    let database_path = tempdir.path().join("verbatim.db");
    let source_path = tempdir.path().join("interrupted-qdrant-upsert.md");
    fs::write(&source_path, "in-flight qdrant source body").unwrap();
    let mut pipeline = IngestPipeline::from_parts(
        Store::new(&database_path).unwrap(),
        HnswIndex::new(),
        StaticEmbeddingClient,
        tempdir.path().to_path_buf(),
    );
    let source_id = pipeline.add_source(&source_path).unwrap();
    pipeline.ingest_source(&source_id).await.unwrap();

    // The server confirms the remote upsert response has been written; dropping
    // the future before polling it again models process exit before compensation.
    let (qdrant_url, upsert_started, release, _stop, server) =
        spawn_pausing_qdrant_upsert_server(4, None);
    pipeline = pipeline.with_qdrant_client(QdrantClient::new(qdrant_test_config(qdrant_url)));
    let mut sync = Box::pin(pipeline.sync_qdrant_source(&source_id));
    tokio::select! {
        _ = &mut sync => panic!("qdrant sync completed before the upsert pause"),
        result = upsert_started => result.unwrap(),
    }
    let store = Store::new(&database_path).unwrap();
    store.remove_source(&source_id).unwrap();
    release.send(()).unwrap();
    let requests = server.join().unwrap();
    assert_eq!(
        requests[3].line,
        "PUT /collections/verbatim/points?wait=true HTTP/1.1"
    );
    drop(sync);
    assert_eq!(
        store.qdrant_deletion_outcome(&source_id).unwrap(),
        Some(DeletionOutcome::Pending)
    );

    let (reconcile_url, _upsert_started, _release, _stop, reconcile_server) =
        spawn_pausing_qdrant_upsert_server(2, None);
    let reconcile_pipeline = IngestPipeline::from_parts(
        Store::new(&database_path).unwrap(),
        HnswIndex::new(),
        StaticEmbeddingClient,
        tempdir.path().to_path_buf(),
    )
    .with_qdrant_client(QdrantClient::new(qdrant_test_config(reconcile_url)));
    assert_eq!(
        reconcile_pipeline
            .reconcile_deletions_up_to(1)
            .await
            .unwrap()
            .len(),
        1
    );
    reconcile_server.join().unwrap();
    assert_eq!(
        store.qdrant_deletion_outcome(&source_id).unwrap(),
        Some(DeletionOutcome::Erased)
    );
}

#[cfg(feature = "qdrant")]
#[tokio::test]
async fn tombstone_fence_continues_full_profile_compensation_after_a_failure() {
    let tempdir = tempfile::tempdir().unwrap();
    let database_path = tempdir.path().join("verbatim.db");
    let mut pipeline = IngestPipeline::from_parts(
        Store::new(&database_path).unwrap(),
        HnswIndex::new(),
        StaticEmbeddingClient,
        tempdir.path().to_path_buf(),
    );
    let source_ids = ["first", "second"]
        .into_iter()
        .map(|name| {
            let source_path = tempdir.path().join(format!("{name}.md"));
            fs::write(&source_path, format!("qdrant source {name}")).unwrap();
            let source_id = pipeline.add_source(&source_path).unwrap();
            source_id
        })
        .collect::<Vec<_>>();
    for source_id in &source_ids {
        pipeline.ingest_source(source_id).await.unwrap();
    }

    let (qdrant_url, upsert_started, release, stop, server) =
        spawn_pausing_qdrant_upsert_server(8, Some(5));
    pipeline = pipeline.with_qdrant_client(QdrantClient::new(qdrant_test_config(qdrant_url)));
    let mut sync =
        Box::pin(pipeline.sync_qdrant_profile_all(pipeline.active_embedding_profile_id()));
    tokio::select! {
        _ = &mut sync => panic!("qdrant full sync completed before the upsert pause"),
        result = upsert_started => result.unwrap(),
    }

    let store = Store::new(&database_path).unwrap();
    for source_id in &source_ids {
        store.remove_source(source_id).unwrap();
        finalize_qdrant_outcome(&store, source_id, DeletionOutcome::Erased);
    }
    release.send(()).unwrap();
    sync.await;
    let _ = stop.send(());

    let requests = server.join().unwrap();
    let compensating_delete_count = requests
        .iter()
        .filter(|request| {
            request.line == "POST /collections/verbatim/points/delete?wait=true HTTP/1.1"
        })
        .count();
    assert_eq!(compensating_delete_count, 3);
    assert_eq!(
        source_ids
            .iter()
            .filter(|source_id| {
                store.qdrant_deletion_outcome(source_id).unwrap() == Some(DeletionOutcome::Pending)
            })
            .count(),
        1
    );
}
