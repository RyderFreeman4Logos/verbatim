//! Optional Qdrant vector index integration.

use std::time::Duration;

use anyhow::{bail, Context, Result};
use reqwest::{Client, Method, Response, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use url::form_urlencoded::byte_serialize;

use crate::config::QdrantConfig;
use crate::store::Store;
use crate::traits::VectorDocument;
use crate::types::{Chunk, ChunkId, EmbeddingProfileId, SourceId};

const DISTANCE: &str = "Cosine";
const MAX_QDRANT_ERROR_BODY_BYTES: u64 = 4096;
const MAX_QDRANT_TEXT_PREVIEW_CHARS: usize = 240;

/// One vector plus the Qdrant payload fields needed for remote search.
#[derive(Clone, Debug, PartialEq)]
pub struct QdrantVectorRecord {
    pub profile_id: EmbeddingProfileId,
    pub document: VectorDocument,
    pub heading_path: Vec<String>,
    pub text_preview: String,
}

impl QdrantVectorRecord {
    pub fn from_chunk(
        profile_id: &EmbeddingProfileId,
        document: VectorDocument,
        chunk: &Chunk,
    ) -> Self {
        Self {
            profile_id: profile_id.clone(),
            document,
            heading_path: chunk.heading_path.clone(),
            text_preview: text_preview(chunk),
        }
    }
}

/// Build Qdrant records from SQLite's authoritative vector table and chunks.
pub fn records_from_store(
    store: &Store,
    source_filter: Option<&SourceId>,
) -> Result<Vec<QdrantVectorRecord>> {
    records_from_store_for_profile(store, &EmbeddingProfileId::default_profile(), source_filter)
}

/// Build Qdrant records for one embedding profile.
pub fn records_from_store_for_profile(
    store: &Store,
    profile_id: &EmbeddingProfileId,
    source_filter: Option<&SourceId>,
) -> Result<Vec<QdrantVectorRecord>> {
    let mut records = Vec::new();
    for document in store.list_vector_documents_for_profile(profile_id)? {
        if source_filter.is_some_and(|source_id| &document.source_id != source_id) {
            continue;
        }
        let Some(chunk) = store.get_chunk(&document.chunk_id)? else {
            tracing::warn!(
                chunk_id = %document.chunk_id.0,
                "stored vector has no chunk row; skipping qdrant sync record"
            );
            continue;
        };
        records.push(QdrantVectorRecord::from_chunk(profile_id, document, &chunk));
    }
    Ok(records)
}

/// Small REST client for Qdrant's collection and points APIs.
#[derive(Clone, Debug)]
pub struct QdrantClient {
    config: QdrantConfig,
    client: Client,
}

impl QdrantClient {
    pub fn from_config(config: &QdrantConfig) -> Option<Self> {
        config.enabled.then(|| Self::new(config.clone()))
    }

    pub fn new(config: QdrantConfig) -> Self {
        Self {
            config,
            client: Client::new(),
        }
    }

    pub async fn delete_source(&self, source_id: &SourceId) -> Result<()> {
        if !self.collection_exists().await? {
            return Ok(());
        }
        let body = QdrantDeleteRequest {
            filter: source_id_filter(source_id),
        };
        let _: QdrantEnvelope<Value> = self
            .send_json(
                Method::POST,
                &self.collection_path("points/delete?wait=true"),
                &body,
                "delete qdrant points by source",
            )
            .await?;
        Ok(())
    }

    pub async fn delete_source_for_profile(
        &self,
        profile_id: &EmbeddingProfileId,
        source_id: &SourceId,
    ) -> Result<()> {
        if !self.collection_exists().await? {
            return Ok(());
        }
        let body = QdrantDeleteRequest {
            filter: profile_source_filter(profile_id, Some(source_id)),
        };
        let _: QdrantEnvelope<Value> = self
            .send_json(
                Method::POST,
                &self.collection_path("points/delete?wait=true"),
                &body,
                "delete qdrant points by embedding profile and source",
            )
            .await?;
        Ok(())
    }

    pub async fn delete_profile(&self, profile_id: &EmbeddingProfileId) -> Result<()> {
        if !self.collection_exists().await? {
            return Ok(());
        }
        let body = QdrantDeleteRequest {
            filter: profile_source_filter(profile_id, None),
        };
        let _: QdrantEnvelope<Value> = self
            .send_json(
                Method::POST,
                &self.collection_path("points/delete?wait=true"),
                &body,
                "delete qdrant points by embedding profile",
            )
            .await?;
        Ok(())
    }

    pub async fn upsert_records(&self, records: &[QdrantVectorRecord]) -> Result<()> {
        if records.is_empty() {
            return Ok(());
        }
        let dimension = records_dimension(records)?;
        self.ensure_collection(dimension).await?;
        self.upsert_records_without_collection_check(records).await
    }

    pub async fn search(
        &self,
        profile_id: &EmbeddingProfileId,
        query: &[f32],
        top_k: usize,
        source_filter: Option<&SourceId>,
    ) -> Result<Vec<(ChunkId, f32)>> {
        if top_k == 0 || query.is_empty() {
            return Ok(Vec::new());
        }
        let body = QdrantSearchRequest {
            vector: query,
            limit: top_k,
            filter: Some(profile_source_filter(profile_id, source_filter)),
            with_payload: ["chunk_id"],
            with_vector: false,
        };
        let response: QdrantEnvelope<Vec<QdrantScoredPoint>> = self
            .send_json(
                Method::POST,
                &self.collection_path("points/search"),
                &body,
                "search qdrant points",
            )
            .await?;
        Ok(response
            .result
            .into_iter()
            .filter_map(|point| chunk_id_from_payload(point.payload).map(|id| (id, point.score)))
            .collect())
    }

    async fn ensure_collection(&self, dimension: usize) -> Result<()> {
        if self.collection_exists().await? {
            return Ok(());
        }
        self.create_collection(dimension).await
    }

    async fn collection_exists(&self) -> Result<bool> {
        let response = self
            .send_without_body(
                Method::GET,
                &self.collection_path(""),
                "check qdrant collection",
            )
            .await?;
        if response.status() == StatusCode::NOT_FOUND {
            return Ok(false);
        }
        self.decode_response::<QdrantEnvelope<Value>>(response, "check qdrant collection")
            .await?;
        Ok(true)
    }

    async fn create_collection(&self, dimension: usize) -> Result<()> {
        let body = QdrantCreateCollectionRequest {
            vectors: QdrantVectorParams {
                size: dimension,
                distance: DISTANCE,
            },
        };
        let _: QdrantEnvelope<bool> = self
            .send_json(
                Method::PUT,
                &self.collection_path(""),
                &body,
                "create qdrant collection",
            )
            .await?;
        Ok(())
    }

    async fn upsert_records_without_collection_check(
        &self,
        records: &[QdrantVectorRecord],
    ) -> Result<()> {
        let points = records
            .iter()
            .map(QdrantPoint::from_record)
            .collect::<Vec<_>>();
        let body = QdrantUpsertRequest { points };
        let _: QdrantEnvelope<Value> = self
            .send_json(
                Method::PUT,
                &self.collection_path("points?wait=true"),
                &body,
                "upsert qdrant points",
            )
            .await?;
        Ok(())
    }

    async fn send_json<T, B>(
        &self,
        method: Method,
        path: &str,
        body: &B,
        operation: &str,
    ) -> Result<T>
    where
        T: DeserializeOwned,
        B: Serialize + ?Sized,
    {
        let response = self
            .request(method, path, operation)?
            .json(body)
            .send()
            .await
            .with_context(|| format!("{operation}: request failed"))?;
        self.decode_response(response, operation).await
    }

    async fn send_without_body(
        &self,
        method: Method,
        path: &str,
        operation: &str,
    ) -> Result<Response> {
        self.request(method, path, operation)?
            .send()
            .await
            .with_context(|| format!("{operation}: request failed"))
    }

    fn request(
        &self,
        method: Method,
        path: &str,
        operation: &str,
    ) -> Result<reqwest::RequestBuilder> {
        let base_url = self.config.url.trim_end_matches('/');
        if base_url.is_empty() {
            bail!("{operation}: qdrant url is empty");
        }
        let url = format!("{base_url}/{path}");
        Ok(self
            .client
            .request(method, url)
            .timeout(Duration::from_secs(self.config.timeout_seconds.max(1))))
    }

    async fn decode_response<T>(&self, response: Response, operation: &str) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let status = response.status();
        if !status.is_success() {
            let body = bounded_response_text(response).await;
            bail!("{operation}: qdrant returned {status}: {body}");
        }
        response
            .json::<T>()
            .await
            .with_context(|| format!("{operation}: decode qdrant response"))
    }

    fn collection_path(&self, suffix: &str) -> String {
        let encoded = byte_serialize(self.config.collection.as_bytes()).collect::<String>();
        if suffix.is_empty() {
            format!("collections/{encoded}")
        } else {
            format!("collections/{encoded}/{}", suffix.trim_start_matches('/'))
        }
    }
}

#[derive(Debug, Serialize)]
struct QdrantCreateCollectionRequest<'a> {
    vectors: QdrantVectorParams<'a>,
}

#[derive(Debug, Serialize)]
struct QdrantVectorParams<'a> {
    size: usize,
    distance: &'a str,
}

#[derive(Debug, Serialize)]
struct QdrantUpsertRequest {
    points: Vec<QdrantPoint>,
}

#[derive(Debug, Serialize)]
struct QdrantPoint {
    id: String,
    vector: Vec<f32>,
    payload: QdrantPayload,
}

impl QdrantPoint {
    fn from_record(record: &QdrantVectorRecord) -> Self {
        Self {
            id: point_id_for_profile_chunk(&record.profile_id, &record.document.chunk_id),
            vector: record.document.vector.clone(),
            payload: QdrantPayload {
                profile_id: record.profile_id.as_str().to_string(),
                chunk_id: record.document.chunk_id.0.clone(),
                source_id: record.document.source_id.0.clone(),
                heading_path: record.heading_path.clone(),
                text_preview: record.text_preview.clone(),
            },
        }
    }
}

#[derive(Debug, Serialize)]
struct QdrantPayload {
    profile_id: String,
    chunk_id: String,
    source_id: String,
    heading_path: Vec<String>,
    text_preview: String,
}

#[derive(Debug, Serialize)]
struct QdrantDeleteRequest<'a> {
    filter: QdrantFilter<'a>,
}

#[derive(Debug, Serialize)]
struct QdrantSearchRequest<'a> {
    vector: &'a [f32],
    limit: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    filter: Option<QdrantFilter<'a>>,
    with_payload: [&'static str; 1],
    with_vector: bool,
}

#[derive(Debug, Serialize)]
struct QdrantFilter<'a> {
    must: Vec<QdrantFieldCondition<'a>>,
}

#[derive(Debug, Serialize)]
struct QdrantFieldCondition<'a> {
    key: &'static str,
    #[serde(rename = "match")]
    match_value: QdrantMatchValue<'a>,
}

#[derive(Debug, Serialize)]
struct QdrantMatchValue<'a> {
    value: &'a str,
}

#[derive(Debug, Deserialize)]
struct QdrantEnvelope<T> {
    result: T,
}

#[derive(Debug, Deserialize)]
struct QdrantScoredPoint {
    score: f32,
    payload: Option<Value>,
}

fn source_id_filter(source_id: &SourceId) -> QdrantFilter<'_> {
    QdrantFilter {
        must: vec![QdrantFieldCondition {
            key: "source_id",
            match_value: QdrantMatchValue {
                value: &source_id.0,
            },
        }],
    }
}

fn profile_source_filter<'a>(
    profile_id: &'a EmbeddingProfileId,
    source_id: Option<&'a SourceId>,
) -> QdrantFilter<'a> {
    let mut must = vec![QdrantFieldCondition {
        key: "profile_id",
        match_value: QdrantMatchValue {
            value: profile_id.as_str(),
        },
    }];
    if let Some(source_id) = source_id {
        must.push(QdrantFieldCondition {
            key: "source_id",
            match_value: QdrantMatchValue {
                value: &source_id.0,
            },
        });
    }
    QdrantFilter { must }
}

fn chunk_id_from_payload(payload: Option<Value>) -> Option<ChunkId> {
    payload?
        .get("chunk_id")
        .and_then(Value::as_str)
        .map(|id| ChunkId(id.to_string()))
}

fn records_dimension(records: &[QdrantVectorRecord]) -> Result<usize> {
    let Some(first) = records.first() else {
        bail!("qdrant sync requires at least one vector record");
    };
    let dimension = first.document.vector.len();
    if dimension == 0 {
        bail!("qdrant sync requires non-empty vectors");
    }
    if records
        .iter()
        .any(|record| record.document.vector.len() != dimension)
    {
        bail!("qdrant sync requires equal vector dimensions");
    }
    Ok(dimension)
}

fn text_preview(chunk: &Chunk) -> String {
    let text = chunk.context_text.as_deref().unwrap_or(&chunk.text);
    text.chars().take(MAX_QDRANT_TEXT_PREVIEW_CHARS).collect()
}

fn point_id_for_profile_chunk(profile_id: &EmbeddingProfileId, chunk_id: &ChunkId) -> String {
    let digest = Sha256::digest(
        format!("verbatim:qdrant:{}:{}", profile_id.as_str(), chunk_id.0).as_bytes(),
    );
    let mut bytes = [0u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    bytes[6] = (bytes[6] & 0x0f) | 0x50;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    format!(
        "{:02x}{:02x}{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}-{:02x}{:02x}{:02x}{:02x}{:02x}{:02x}",
        bytes[0],
        bytes[1],
        bytes[2],
        bytes[3],
        bytes[4],
        bytes[5],
        bytes[6],
        bytes[7],
        bytes[8],
        bytes[9],
        bytes[10],
        bytes[11],
        bytes[12],
        bytes[13],
        bytes[14],
        bytes[15]
    )
}

async fn bounded_response_text(mut response: Response) -> String {
    let mut body = String::new();
    while (body.len() as u64) < MAX_QDRANT_ERROR_BODY_BYTES {
        let chunk = match response.chunk().await {
            Ok(Some(chunk)) => chunk,
            Ok(None) | Err(_) => break,
        };
        let remaining = (MAX_QDRANT_ERROR_BODY_BYTES as usize).saturating_sub(body.len());
        body.push_str(&String::from_utf8_lossy(
            &chunk[..chunk.len().min(remaining)],
        ));
    }
    body
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    use super::*;

    #[derive(Debug)]
    struct TestRequest {
        line: String,
        body: String,
    }

    fn qdrant_config(url: String) -> QdrantConfig {
        QdrantConfig {
            enabled: true,
            url,
            collection: "verbatim".into(),
            prefer_for_search: true,
            timeout_seconds: 2,
        }
    }

    fn record(source_id: &str, chunk_id: &str, vector: Vec<f32>) -> QdrantVectorRecord {
        QdrantVectorRecord {
            profile_id: EmbeddingProfileId::default_profile(),
            document: VectorDocument {
                chunk_id: ChunkId(chunk_id.into()),
                source_id: SourceId(source_id.into()),
                vector,
            },
            heading_path: vec!["Intro".into()],
            text_preview: "preview text".into(),
        }
    }

    fn spawn_server(
        responses: Vec<(u16, &'static str)>,
    ) -> (String, thread::JoinHandle<Vec<TestRequest>>) {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind test server");
        let addr = listener.local_addr().expect("test server addr");
        let handle = thread::spawn(move || {
            let mut requests = Vec::new();
            for response in responses {
                let (mut stream, _) = listener.accept().expect("accept request");
                requests.push(read_request(&mut stream));
                write_response(&mut stream, response.0, response.1);
            }
            requests
        });
        (format!("http://{addr}"), handle)
    }

    fn read_request(stream: &mut TcpStream) -> TestRequest {
        let mut buffer = Vec::new();
        let mut chunk = [0u8; 1024];
        loop {
            let n = stream.read(&mut chunk).expect("read request");
            if n == 0 {
                break;
            }
            buffer.extend_from_slice(&chunk[..n]);
            if request_complete(&buffer) {
                break;
            }
        }
        let text = String::from_utf8(buffer).expect("request utf8");
        let (head, body) = text.split_once("\r\n\r\n").unwrap_or((&text, ""));
        let line = head.lines().next().unwrap_or_default().to_string();
        TestRequest {
            line,
            body: body.to_string(),
        }
    }

    fn request_complete(buffer: &[u8]) -> bool {
        let text = String::from_utf8_lossy(buffer);
        let Some((head, body)) = text.split_once("\r\n\r\n") else {
            return false;
        };
        let content_len = head
            .lines()
            .find_map(|line| {
                let (name, value) = line.split_once(':')?;
                name.eq_ignore_ascii_case("content-length")
                    .then(|| value.trim().parse::<usize>().ok())
                    .flatten()
            })
            .unwrap_or(0);
        body.len() >= content_len
    }

    fn write_response(stream: &mut TcpStream, status: u16, body: &str) {
        let reason = if status == 200 { "OK" } else { "ERR" };
        write!(
            stream,
            "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{}",
            body.len(),
            body
        )
        .expect("write response");
    }

    #[test]
    fn point_id_is_deterministic_uuid_from_profile_and_chunk_id() {
        let default_profile = EmbeddingProfileId::default_profile();
        let alt_profile = EmbeddingProfileId::new("alt").unwrap();
        let first = point_id_for_profile_chunk(&default_profile, &ChunkId("src-child-0".into()));
        let second = point_id_for_profile_chunk(&default_profile, &ChunkId("src-child-0".into()));
        let different_chunk =
            point_id_for_profile_chunk(&default_profile, &ChunkId("src-child-1".into()));
        let different_profile =
            point_id_for_profile_chunk(&alt_profile, &ChunkId("src-child-0".into()));

        assert_eq!(first, second);
        assert_ne!(first, different_chunk);
        assert_ne!(first, different_profile);
        assert_eq!(first.len(), 36);
        assert_eq!(first.as_bytes()[14], b'5');
    }

    #[tokio::test]
    async fn upsert_records_creates_collection_and_sends_payload() {
        let (url, handle) = spawn_server(vec![
            (404, r#"{"status":{"error":"missing"},"result":null}"#),
            (200, r#"{"status":"ok","result":true}"#),
            (
                200,
                r#"{"status":"ok","result":{"status":"acknowledged","operation_id":1}}"#,
            ),
        ]);
        let client = QdrantClient::new(qdrant_config(url));

        client
            .upsert_records(&[record("src-1", "src-1-child-0", vec![0.1, 0.2])])
            .await
            .unwrap();

        let requests = handle.join().unwrap();
        assert_eq!(requests[0].line, "GET /collections/verbatim HTTP/1.1");
        assert_eq!(requests[1].line, "PUT /collections/verbatim HTTP/1.1");
        let create: Value = serde_json::from_str(&requests[1].body).unwrap();
        assert_eq!(create["vectors"]["size"], 2);
        assert_eq!(create["vectors"]["distance"], "Cosine");
        assert_eq!(
            requests[2].line,
            "PUT /collections/verbatim/points?wait=true HTTP/1.1"
        );
        let upsert: Value = serde_json::from_str(&requests[2].body).unwrap();
        assert_eq!(upsert["points"][0]["payload"]["profile_id"], "default");
        assert_eq!(upsert["points"][0]["payload"]["chunk_id"], "src-1-child-0");
        assert_eq!(upsert["points"][0]["payload"]["source_id"], "src-1");
        assert_eq!(upsert["points"][0]["payload"]["heading_path"][0], "Intro");
        assert_eq!(
            upsert["points"][0]["payload"]["text_preview"],
            "preview text"
        );
        assert_ne!(upsert["points"][0]["id"], "src-1-child-0");
    }

    #[tokio::test]
    async fn search_sends_source_filter_and_maps_payload_chunk_ids() {
        let (url, handle) = spawn_server(vec![(
            200,
            r#"{"status":"ok","result":[{"id":"550e8400-e29b-41d4-a716-446655440000","score":0.75,"payload":{"chunk_id":"chunk-a"}}]}"#,
        )]);
        let client = QdrantClient::new(qdrant_config(url));
        let alt_profile = EmbeddingProfileId::new("alt").unwrap();

        let hits = client
            .search(
                &alt_profile,
                &[0.3, 0.4],
                7,
                Some(&SourceId("src-1".into())),
            )
            .await
            .unwrap();

        assert_eq!(hits, vec![(ChunkId("chunk-a".into()), 0.75)]);
        let requests = handle.join().unwrap();
        assert_eq!(
            requests[0].line,
            "POST /collections/verbatim/points/search HTTP/1.1"
        );
        let body: Value = serde_json::from_str(&requests[0].body).unwrap();
        assert_eq!(body["limit"], 7);
        assert_eq!(body["filter"]["must"][0]["key"], "profile_id");
        assert_eq!(body["filter"]["must"][0]["match"]["value"], "alt");
        assert_eq!(body["filter"]["must"][1]["key"], "source_id");
        assert_eq!(body["filter"]["must"][1]["match"]["value"], "src-1");
        assert_eq!(body["with_payload"][0], "chunk_id");
        assert_eq!(body["with_vector"], false);
    }

    #[tokio::test]
    async fn delete_source_uses_payload_filter() {
        let (url, handle) = spawn_server(vec![
            (200, r#"{"status":"ok","result":{}}"#),
            (
                200,
                r#"{"status":"ok","result":{"status":"acknowledged","operation_id":2}}"#,
            ),
        ]);
        let client = QdrantClient::new(qdrant_config(url));

        client
            .delete_source(&SourceId("src-1".into()))
            .await
            .unwrap();

        let requests = handle.join().unwrap();
        assert_eq!(
            requests[1].line,
            "POST /collections/verbatim/points/delete?wait=true HTTP/1.1"
        );
        let body: Value = serde_json::from_str(&requests[1].body).unwrap();
        assert_eq!(body["filter"]["must"][0]["key"], "source_id");
        assert_eq!(body["filter"]["must"][0]["match"]["value"], "src-1");
    }

    #[tokio::test]
    async fn delete_source_for_profile_uses_profile_and_source_filters() {
        let (url, handle) = spawn_server(vec![
            (200, r#"{"status":"ok","result":{}}"#),
            (
                200,
                r#"{"status":"ok","result":{"status":"acknowledged","operation_id":3}}"#,
            ),
        ]);
        let client = QdrantClient::new(qdrant_config(url));
        let alt_profile = EmbeddingProfileId::new("alt").unwrap();

        client
            .delete_source_for_profile(&alt_profile, &SourceId("src-1".into()))
            .await
            .unwrap();

        let requests = handle.join().unwrap();
        let body: Value = serde_json::from_str(&requests[1].body).unwrap();
        assert_eq!(body["filter"]["must"][0]["key"], "profile_id");
        assert_eq!(body["filter"]["must"][0]["match"]["value"], "alt");
        assert_eq!(body["filter"]["must"][1]["key"], "source_id");
        assert_eq!(body["filter"]["must"][1]["match"]["value"], "src-1");
    }
}
