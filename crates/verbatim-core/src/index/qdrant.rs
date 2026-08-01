//! Optional Qdrant vector index integration.

use std::time::Duration;

use anyhow::{bail, Result};
use reqwest::{Client, Method, Response, StatusCode};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use url::form_urlencoded::byte_serialize;

use crate::config::QdrantConfig;
use crate::store::Store;
use crate::traits::VectorDocument;
use crate::types::{Chunk, ChunkId, EmbeddingProfileId, SourceId};
use crate::upstream::{
    capture_full_response, capture_response_prefix, UpstreamFailureError, UpstreamRequestContext,
    DEFAULT_BODY_PREFIX_MAX_BYTES,
};

const DISTANCE: &str = "Cosine";
const MAX_QDRANT_TEXT_PREVIEW_CHARS: usize = 240;

/// One vector plus the Qdrant payload fields needed for remote search.
#[derive(Clone, Debug, PartialEq)]
pub struct QdrantVectorRecord {
    pub profile_id: EmbeddingProfileId,
    pub profile_generation: u64,
    pub document: VectorDocument,
    pub heading_path: Vec<String>,
    pub text_preview: String,
}

impl QdrantVectorRecord {
    pub fn from_chunk(
        profile_id: &EmbeddingProfileId,
        profile_generation: u64,
        document: VectorDocument,
        chunk: &Chunk,
    ) -> Self {
        Self {
            profile_id: profile_id.clone(),
            profile_generation,
            document,
            heading_path: chunk.heading_path.clone(),
            text_preview: text_preview(chunk),
        }
    }
}

/// Remote dense hit plus the stored point identity needed for authoritative validation.
#[derive(Clone, Debug, PartialEq)]
pub struct QdrantHit {
    pub point_id: String,
    pub chunk_id: ChunkId,
    pub profile_id: EmbeddingProfileId,
    pub source_id: SourceId,
    pub score: f32,
    pub profile_generation: u64,
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
    let profile_generation = store.index_generation_for_profile(profile_id)?;
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
        records.push(QdrantVectorRecord::from_chunk(
            profile_id,
            profile_generation,
            document,
            &chunk,
        ));
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
    ) -> Result<Vec<QdrantHit>> {
        if top_k == 0 || query.is_empty() {
            return Ok(Vec::new());
        }
        let body = QdrantSearchRequest {
            vector: query,
            limit: top_k,
            filter: Some(profile_source_filter(profile_id, source_filter)),
            with_payload: ["chunk_id", "profile_generation", "profile_id", "source_id"],
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
            .filter_map(|point| hit_from_payload(point.id, point.payload, point.score))
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
        if response.response.status() == StatusCode::NOT_FOUND {
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
        let request = self.request(method, path, operation)?;
        let context = request.context.clone();
        let response = request
            .builder
            .json(body)
            .send()
            .await
            .map_err(|source| qdrant_transport_error(operation, &context, source))?;
        self.decode_response(QdrantRawResponse { response, context }, operation)
            .await
    }

    async fn send_without_body(
        &self,
        method: Method,
        path: &str,
        operation: &str,
    ) -> Result<QdrantRawResponse> {
        let request = self.request(method, path, operation)?;
        let context = request.context.clone();
        let response = request
            .builder
            .send()
            .await
            .map_err(|source| qdrant_transport_error(operation, &context, source))?;
        Ok(QdrantRawResponse { response, context })
    }

    fn request(&self, method: Method, path: &str, operation: &str) -> Result<QdrantRequest> {
        let base_url = self.config.url.trim_end_matches('/');
        if base_url.is_empty() {
            bail!("{operation}: qdrant url is empty");
        }
        let url = format!("{base_url}/{path}");
        let context = UpstreamRequestContext::new(
            operation,
            "qdrant",
            Some("qdrant".into()),
            None,
            &method,
            &url,
        );
        let builder = self
            .client
            .request(method, url)
            .timeout(Duration::from_secs(self.config.timeout_seconds.max(1)));
        Ok(QdrantRequest { builder, context })
    }

    async fn decode_response<T>(&self, raw: QdrantRawResponse, operation: &str) -> Result<T>
    where
        T: DeserializeOwned,
    {
        let status = raw.response.status();
        if !status.is_success() {
            let captured =
                capture_response_prefix(raw.response, DEFAULT_BODY_PREFIX_MAX_BYTES).await;
            let diagnostic = captured.diagnostic_for_status(&raw.context);
            let message = format!("{operation}: qdrant returned {status}");
            return Err(UpstreamFailureError::new(message, diagnostic).into());
        }
        let headers = raw.response.headers().clone();
        let captured = capture_full_response(raw.response)
            .await
            .map_err(|source| {
                let diagnostic = raw
                    .context
                    .body_read_failure(Some(status), &headers, &source);
                UpstreamFailureError::new(
                    format!("{operation}: read qdrant response body"),
                    diagnostic,
                )
            })?;
        serde_json::from_slice::<T>(&captured.body).map_err(|source| {
            let diagnostic = captured.diagnostic_for_decode(&raw.context, &source);
            UpstreamFailureError::new(format!("{operation}: decode qdrant response"), diagnostic)
                .into()
        })
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

struct QdrantRequest {
    builder: reqwest::RequestBuilder,
    context: UpstreamRequestContext,
}

struct QdrantRawResponse {
    response: Response,
    context: UpstreamRequestContext,
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
                profile_generation: record.profile_generation,
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
    profile_generation: u64,
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
    with_payload: [&'static str; 4],
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
    id: Option<Value>,
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

fn hit_from_payload(
    point_id: Option<Value>,
    payload: Option<Value>,
    score: f32,
) -> Option<QdrantHit> {
    let point_id = point_id?.as_str()?.to_string();
    let payload = payload?;
    let chunk_id = ChunkId(payload.get("chunk_id")?.as_str()?.to_string());
    let profile_id = EmbeddingProfileId::new(payload.get("profile_id")?.as_str()?).ok()?;
    let source_id = SourceId(payload.get("source_id")?.as_str()?.to_string());
    let profile_generation = payload.get("profile_generation").and_then(Value::as_u64)?;
    if chunk_id.0.is_empty()
        || source_id.0.is_empty()
        || point_id != point_id_for_profile_chunk(&profile_id, &chunk_id)
    {
        return None;
    }
    Some(QdrantHit {
        point_id,
        chunk_id,
        profile_id,
        source_id,
        score,
        profile_generation,
    })
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
    let [a, b, c, d, e, f, g, h, i, j, k, l, m, n, o, p] = bytes;
    format!("{a:02x}{b:02x}{c:02x}{d:02x}-{e:02x}{f:02x}-{g:02x}{h:02x}-{i:02x}{j:02x}-{k:02x}{l:02x}{m:02x}{n:02x}{o:02x}{p:02x}")
}

fn qdrant_transport_error(
    operation: &str,
    context: &UpstreamRequestContext,
    source: reqwest::Error,
) -> anyhow::Error {
    let diagnostic = context.transport_failure(&source);
    UpstreamFailureError::new(format!("{operation}: request failed"), diagnostic).into()
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
            profile_generation: 7,
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
        assert_eq!(upsert["points"][0]["payload"]["profile_generation"], 7);
        assert_eq!(upsert["points"][0]["payload"]["chunk_id"], "src-1-child-0");
        assert_eq!(upsert["points"][0]["payload"]["source_id"], "src-1");
        assert_eq!(upsert["points"][0]["payload"]["heading_path"][0], "Intro");
        assert_eq!(
            upsert["points"][0]["payload"]["text_preview"],
            "preview text"
        );
        assert_ne!(upsert["points"][0]["id"], "src-1-child-0");
    }

    include!("qdrant/search_identity_tests.rs");

    #[tokio::test]
    async fn search_invalid_json_error_exposes_upstream_diagnostic() {
        let (url, handle) = spawn_server(vec![(200, "not-json")]);
        let client = QdrantClient::new(qdrant_config(url));

        let error = client
            .search(&EmbeddingProfileId::default_profile(), &[0.3, 0.4], 7, None)
            .await
            .expect_err("invalid qdrant json fails");
        let _requests = handle.join().unwrap();

        let diagnostic = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<UpstreamFailureError>())
            .expect("upstream failure in error chain")
            .diagnostic();
        assert_eq!(diagnostic.client_kind, "qdrant");
        assert_eq!(diagnostic.status_code, Some(200));
        assert_eq!(
            diagnostic.endpoint_path,
            "/collections/verbatim/points/search"
        );
        assert_eq!(diagnostic.response_body_prefix.as_deref(), Some("not-json"));
        assert_eq!(
            diagnostic.transport_error_kind.as_deref(),
            Some("invalid_json")
        );
    }

    #[tokio::test]
    async fn search_http_status_diagnostic_redacts_qdrant_body() {
        let body = concat!(
            "Authorization: Bearer fixturebearertoken\n",
            "token=fixture12345 ",
            "OPENAI_API_KEY=providerfixture12345 ",
            "https://user:pass@example.test/path?token=fixture12345"
        );
        let (url, handle) = spawn_server(vec![(502, body)]);
        let client = QdrantClient::new(qdrant_config(url));

        let error = client
            .search(&EmbeddingProfileId::default_profile(), &[0.3, 0.4], 7, None)
            .await
            .expect_err("qdrant status fails");
        let _requests = handle.join().unwrap();

        let diagnostic = error
            .chain()
            .find_map(|cause| cause.downcast_ref::<UpstreamFailureError>())
            .expect("upstream failure in error chain")
            .diagnostic();
        let encoded = serde_json::to_string(diagnostic).unwrap();
        assert_eq!(diagnostic.status_code, Some(502));
        assert!(encoded.contains("<redacted>"));
        assert!(!encoded.contains("fixturebearertoken"));
        assert!(!encoded.contains("fixture12345"));
        assert!(!encoded.contains("providerfixture12345"));
        assert!(!encoded.contains("user:pass"));
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
