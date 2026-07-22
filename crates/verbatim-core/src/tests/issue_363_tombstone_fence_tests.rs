use super::*;
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
use std::{sync::mpsc, thread};

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
fn spawn_pausing_qdrant_upsert_server() -> (
    String,
    tokio::sync::oneshot::Receiver<()>,
    mpsc::Sender<()>,
    thread::JoinHandle<Vec<TestHttpRequest>>,
) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let address = listener.local_addr().unwrap();
    let (upsert_started_tx, upsert_started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let handle = thread::spawn(move || {
        let mut upsert_started_tx = Some(upsert_started_tx);
        let mut requests = Vec::new();
        for request_index in 0..6 {
            let (mut stream, _) = listener.accept().unwrap();
            requests.push(read_http_request(&mut stream));
            if request_index == 3 {
                upsert_started_tx.take().unwrap().send(()).unwrap();
                release_rx.recv().unwrap();
            }
            write_http_response(&mut stream, r#"{"status":"ok","result":{}}"#);
        }
        requests
    });
    (
        format!("http://{address}"),
        upsert_started_rx,
        release_tx,
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
fn write_http_response(stream: &mut TcpStream, body: &str) {
    write!(
        stream,
        "HTTP/1.1 200 OK\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
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

    let (qdrant_url, upsert_started, release, server) = spawn_pausing_qdrant_upsert_server();
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
