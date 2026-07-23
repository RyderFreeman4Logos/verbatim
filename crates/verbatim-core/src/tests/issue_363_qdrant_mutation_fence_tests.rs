#[cfg(feature = "qdrant")]
use super::*;
#[cfg(feature = "qdrant")]
use crate::deletion::{DeletionOutcome, DeletionProduct};
#[cfg(feature = "qdrant")]
use crate::resource::{ObservableResource, ResourceLimitConfig};
#[cfg(feature = "qdrant")]
use crate::{config::QdrantConfig, index::qdrant::QdrantClient};
#[cfg(feature = "qdrant")]
use async_trait::async_trait;
#[cfg(feature = "qdrant")]
use std::io::{Read, Write};
#[cfg(feature = "qdrant")]
use std::net::{TcpListener, TcpStream};
#[cfg(feature = "qdrant")]
use std::sync::{mpsc, Arc, Mutex};
#[cfg(feature = "qdrant")]
use std::{thread, time::Duration};

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

#[cfg(feature = "qdrant")]
struct QdrantUpsertCapacityGuard {
    resource: Arc<ObservableResource>,
}

#[cfg(feature = "qdrant")]
impl QdrantUpsertCapacityGuard {
    fn capacity_two() -> Self {
        let resource = ingest_resource("qdrant_upsert", "qdrant_upsert");
        resource.configure(ResourceLimitConfig {
            capacity: 2,
            queue_capacity: 128,
            queue_timeout: Duration::from_secs(5),
        });
        assert_eq!(resource.snapshot().capacity, 2);
        Self { resource }
    }
}

#[cfg(feature = "qdrant")]
impl Drop for QdrantUpsertCapacityGuard {
    fn drop(&mut self) {
        self.resource.configure(ResourceLimitConfig {
            capacity: 1,
            queue_capacity: 512,
            queue_timeout: Duration::from_secs(300),
        });
    }
}

#[cfg(feature = "qdrant")]
#[derive(Debug)]
struct TestHttpRequest {
    line: String,
}

#[cfg(feature = "qdrant")]
struct QdrantMutationFenceServer {
    url: String,
    upsert_started: Option<tokio::sync::oneshot::Receiver<()>>,
    release_upsert: Option<mpsc::Sender<()>>,
    deletion_started: Option<tokio::sync::oneshot::Receiver<()>>,
    release_deletion: Option<mpsc::Sender<()>>,
    stop: Option<mpsc::Sender<()>>,
    handle: Option<thread::JoinHandle<Vec<TestHttpRequest>>>,
}

#[cfg(feature = "qdrant")]
impl QdrantMutationFenceServer {
    fn spawn() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        listener.set_nonblocking(true).unwrap();
        let address = listener.local_addr().unwrap();
        let (upsert_started_tx, upsert_started_rx) = tokio::sync::oneshot::channel();
        let (release_upsert_tx, release_upsert_rx) = mpsc::channel();
        let (deletion_started_tx, deletion_started_rx) = tokio::sync::oneshot::channel();
        let (release_deletion_tx, release_deletion_rx) = mpsc::channel();
        let (stop_tx, stop_rx) = mpsc::channel();
        let handle = thread::spawn(move || {
            let requests = Arc::new(Mutex::new(Vec::new()));
            let mut handlers = Vec::new();
            let mut upsert_started_tx = Some(upsert_started_tx);
            let mut release_upsert_rx = Some(release_upsert_rx);
            let mut deletion_started_tx = Some(deletion_started_tx);
            let mut release_deletion_rx = Some(release_deletion_rx);
            let mut request_index = 0;

            while request_index < 8 {
                match listener.accept() {
                    Ok((mut stream, _)) => {
                        let current_index = request_index;
                        request_index += 1;
                        let request = read_http_request(&mut stream);
                        let requests = Arc::clone(&requests);
                        let upsert_started =
                            (current_index == 3).then(|| upsert_started_tx.take().unwrap());
                        let release_upsert =
                            (current_index == 3).then(|| release_upsert_rx.take().unwrap());
                        let deletion_started =
                            (current_index == 7).then(|| deletion_started_tx.take().unwrap());
                        let release_deletion =
                            (current_index == 7).then(|| release_deletion_rx.take().unwrap());
                        handlers.push(thread::spawn(move || {
                            if let Some(started) = upsert_started {
                                started.send(()).unwrap();
                            }
                            if let Some(release) = release_upsert {
                                let _ = release.recv();
                            }
                            if let Some(started) = deletion_started {
                                started.send(()).unwrap();
                            }
                            if let Some(release) = release_deletion {
                                let _ = release.recv();
                            }
                            let _ = write_http_response(&mut stream);
                            requests.lock().unwrap().push((current_index, request));
                        }));
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

            for handler in handlers {
                handler.join().unwrap();
            }
            let mut requests = requests.lock().unwrap().drain(..).collect::<Vec<_>>();
            requests.sort_by_key(|(index, _)| *index);
            requests.into_iter().map(|(_, request)| request).collect()
        });
        Self {
            url: format!("http://{address}"),
            upsert_started: Some(upsert_started_rx),
            release_upsert: Some(release_upsert_tx),
            deletion_started: Some(deletion_started_rx),
            release_deletion: Some(release_deletion_tx),
            stop: Some(stop_tx),
            handle: Some(handle),
        }
    }

    async fn wait_for_upsert(&mut self) {
        self.upsert_started.take().unwrap().await.unwrap();
    }

    fn release_upsert(&mut self) {
        self.release_upsert.take().unwrap().send(()).unwrap();
    }

    async fn wait_for_deletion(&mut self) {
        self.deletion_started.take().unwrap().await.unwrap();
    }

    fn release_deletion(&mut self) {
        self.release_deletion.take().unwrap().send(()).unwrap();
    }

    fn finish(mut self) -> Vec<TestHttpRequest> {
        self.handle.take().unwrap().join().unwrap()
    }
}

#[cfg(feature = "qdrant")]
impl Drop for QdrantMutationFenceServer {
    fn drop(&mut self) {
        if let Some(release) = self.release_upsert.take() {
            let _ = release.send(());
        }
        if let Some(release) = self.release_deletion.take() {
            let _ = release.send(());
        }
        if let Some(stop) = self.stop.take() {
            let _ = stop.send(());
        }
        if let Some(handle) = self.handle.take() {
            let _ = handle.join();
        }
    }
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
fn read_http_request(stream: &mut TcpStream) -> TestHttpRequest {
    let mut buffer = Vec::new();
    let mut chunk = [0_u8; 1024];
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
    TestHttpRequest {
        line: text.lines().next().unwrap_or_default().to_owned(),
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
fn write_http_response(stream: &mut TcpStream) -> std::io::Result<()> {
    write!(
        stream,
        "HTTP/1.1 200 Test\r\nContent-Length: 27\r\nConnection: close\r\n\r\n{{\"status\":\"ok\",\"result\":{{}}}}"
    )?;
    stream.flush()
}

#[cfg(feature = "qdrant")]
async fn assert_dropped_sync_precedes_erasure(
    sync: impl std::future::Future<Output = ()>,
    delete_pipeline: &mut IngestPipeline<StaticEmbeddingClient>,
    database_path: &std::path::Path,
    source_id: &SourceId,
    server: &mut QdrantMutationFenceServer,
) {
    let mut sync = Box::pin(sync);
    tokio::select! {
        () = &mut sync => panic!("the stale Qdrant upsert returned before the test server paused it"),
        () = server.wait_for_upsert() => {}
    }
    assert!(
        qdrant_mutation_fence().try_write().is_err(),
        "the paused stale upsert must hold the shared Qdrant mutation fence"
    );

    let mut deletion = Box::pin(delete_pipeline.remove_source(source_id));
    assert!(
        matches!(futures::poll!(deletion.as_mut()), std::task::Poll::Pending),
        "the source deletion must queue while the stale Qdrant upsert holds the fence"
    );
    // The server accepted the stale upsert and keeps applying it after this
    // caller gives up. Its lease and tombstone compensation must outlive this
    // future so the terminal deletion cannot pass the still-running mutation.
    drop(sync);
    assert!(
        qdrant_mutation_fence().try_write().is_err(),
        "dropping the caller must not release the stale Qdrant upsert lease before compensation"
    );
    assert_eq!(
        Store::open_existing_readonly(database_path)
            .unwrap()
            .qdrant_deletion_outcome(source_id)
            .unwrap(),
        Some(DeletionOutcome::Pending),
        "the deletion receipt must remain pending while the cancelled caller's upsert is in flight"
    );
    server.release_upsert();
    tokio::select! {
        result = &mut deletion => panic!("the deletion returned before its Qdrant erase request: {result:?}"),
        () = server.wait_for_deletion() => {}
    }
    assert!(
        qdrant_mutation_fence().try_read().is_err(),
        "the remote deletion must retain the exclusive Qdrant mutation fence"
    );
    server.release_deletion();
    let report = deletion.await.unwrap();
    assert_eq!(
        report.status_for(DeletionProduct::Qdrant),
        Some(DeletionOutcome::Erased)
    );
}

#[cfg(feature = "qdrant")]
#[tokio::test]
async fn stale_source_upsert_survives_caller_cancellation_before_erasure() {
    let _capacity = QdrantUpsertCapacityGuard::capacity_two();
    let tempdir = tempfile::tempdir().unwrap();
    let database_path = tempdir.path().join("verbatim.db");
    let source_path = tempdir.path().join("source-sync.md");
    fs::write(&source_path, "source sync mutation fence").unwrap();
    let mut sync_pipeline = IngestPipeline::from_parts(
        Store::new(&database_path).unwrap(),
        HnswIndex::new(),
        StaticEmbeddingClient,
        tempdir.path().to_path_buf(),
    );
    let source_id = sync_pipeline.add_source(&source_path).unwrap();
    sync_pipeline.ingest_source(&source_id).await.unwrap();

    let mut server = QdrantMutationFenceServer::spawn();
    sync_pipeline =
        sync_pipeline.with_qdrant_client(QdrantClient::new(qdrant_test_config(server.url.clone())));
    let mut delete_pipeline = IngestPipeline::from_parts(
        Store::new(&database_path).unwrap(),
        HnswIndex::new(),
        StaticEmbeddingClient,
        tempdir.path().to_path_buf(),
    )
    .with_qdrant_client(QdrantClient::new(qdrant_test_config(server.url.clone())));

    assert_dropped_sync_precedes_erasure(
        sync_pipeline.sync_qdrant_source(&source_id),
        &mut delete_pipeline,
        &database_path,
        &source_id,
        &mut server,
    )
    .await;

    let requests = server.finish();
    assert_eq!(
        requests[3].line,
        "PUT /collections/verbatim/points?wait=true HTTP/1.1"
    );
    assert_eq!(
        requests[5].line,
        "POST /collections/verbatim/points/delete?wait=true HTTP/1.1"
    );
    assert_eq!(
        requests[7].line,
        "POST /collections/verbatim/points/delete?wait=true HTTP/1.1"
    );
    assert_eq!(
        Store::new(&database_path)
            .unwrap()
            .qdrant_deletion_outcome(&source_id)
            .unwrap(),
        Some(DeletionOutcome::Erased)
    );
}

#[cfg(feature = "qdrant")]
#[tokio::test]
async fn capacity_two_full_profile_sync_crash_cannot_leave_a_qdrant_resurrection_after_erased() {
    let _capacity = QdrantUpsertCapacityGuard::capacity_two();
    let tempdir = tempfile::tempdir().unwrap();
    let database_path = tempdir.path().join("verbatim.db");
    let source_path = tempdir.path().join("full-profile-sync.md");
    fs::write(&source_path, "full profile sync mutation fence").unwrap();
    let mut sync_pipeline = IngestPipeline::from_parts(
        Store::new(&database_path).unwrap(),
        HnswIndex::new(),
        StaticEmbeddingClient,
        tempdir.path().to_path_buf(),
    );
    let source_id = sync_pipeline.add_source(&source_path).unwrap();
    sync_pipeline.ingest_source(&source_id).await.unwrap();

    let mut server = QdrantMutationFenceServer::spawn();
    sync_pipeline =
        sync_pipeline.with_qdrant_client(QdrantClient::new(qdrant_test_config(server.url.clone())));
    let mut delete_pipeline = IngestPipeline::from_parts(
        Store::new(&database_path).unwrap(),
        HnswIndex::new(),
        StaticEmbeddingClient,
        tempdir.path().to_path_buf(),
    )
    .with_qdrant_client(QdrantClient::new(qdrant_test_config(server.url.clone())));

    assert_dropped_sync_precedes_erasure(
        sync_pipeline.sync_qdrant_profile_all(sync_pipeline.active_embedding_profile_id()),
        &mut delete_pipeline,
        &database_path,
        &source_id,
        &mut server,
    )
    .await;

    let requests = server.finish();
    assert_eq!(
        requests[3].line,
        "PUT /collections/verbatim/points?wait=true HTTP/1.1"
    );
    assert_eq!(
        requests[5].line,
        "POST /collections/verbatim/points/delete?wait=true HTTP/1.1"
    );
    assert_eq!(
        requests[7].line,
        "POST /collections/verbatim/points/delete?wait=true HTTP/1.1"
    );
    assert_eq!(
        Store::new(&database_path)
            .unwrap()
            .qdrant_deletion_outcome(&source_id)
            .unwrap(),
        Some(DeletionOutcome::Erased)
    );
}
