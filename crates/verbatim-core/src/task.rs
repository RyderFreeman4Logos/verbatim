//! Persistent task metadata shared by daemon, CLI, and storage code.
//!
//! Task records intentionally store bounded execution metadata, not raw user
//! prompts or raw model responses. Callers that need full ask answers must use
//! the synchronous/streaming API response path.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::types::{hex_sha256, EmbeddingCacheStats};

static TASK_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

pub const TASK_METADATA_MAX_BYTES: usize = 8192;
pub const TASK_EVENT_MESSAGE_MAX_CHARS: usize = 512;
pub const TASK_ERROR_MAX_CHARS: usize = 2048;
const TASK_STRING_MAX_CHARS: usize = 256;
const TASK_UPSTREAM_BODY_PREFIX_MAX_CHARS: usize = 4096;
const TASK_ARRAY_MAX_ITEMS: usize = 32;
const TASK_OBJECT_MAX_KEYS: usize = 32;

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct TaskId(pub String);

impl TaskId {
    pub fn new() -> Self {
        let nonce = TASK_ID_COUNTER.fetch_add(1, Ordering::Relaxed);
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|duration| duration.as_nanos())
            .unwrap_or_default();
        let digest = hex_sha256(format!("{now}:{}:{nonce}", std::process::id()).as_bytes());
        Self(format!("task-{}", &digest[..20]))
    }
}

impl Default for TaskId {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskKind {
    Ask,
    Ingest,
    Retrieve,
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Ingest => "ingest",
            Self::Retrieve => "retrieve",
        }
    }

    pub fn from_store_str(value: &str) -> Option<Self> {
        match value {
            "ask" => Some(Self::Ask),
            "ingest" => Some(Self::Ingest),
            "retrieve" => Some(Self::Retrieve),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TaskStatus {
    Queued,
    Running,
    Succeeded,
    Failed,
    Cancelled,
}

impl TaskStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::Succeeded => "succeeded",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    pub fn from_store_str(value: &str) -> Option<Self> {
        match value {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "succeeded" => Some(Self::Succeeded),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            _ => None,
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Succeeded | Self::Failed | Self::Cancelled)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSummary {
    pub id: TaskId,
    pub kind: TaskKind,
    pub status: TaskStatus,
    pub created_at: String,
    pub updated_at: String,
    pub started_at: Option<String>,
    pub finished_at: Option<String>,
    pub request: Value,
    pub result: Option<Value>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_position: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub progress: Option<TaskProgressSnapshot>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskEvent {
    pub sequence: i64,
    pub task_id: TaskId,
    pub event_type: String,
    pub message: String,
    pub payload: Value,
    pub created_at: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSpan {
    pub sequence: i64,
    pub task_id: TaskId,
    pub phase: String,
    pub started_at: String,
    pub duration_ms: u64,
    pub metadata: Value,
}

#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TaskProgressSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub phase: Option<TaskProgressPhase>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub counters: Vec<TaskProgressCounter>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub endpoints: Vec<TaskEndpointSummary>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue: Option<TaskQueueProgress>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_worker_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_status: Option<String>,
}

impl TaskProgressSnapshot {
    pub fn phase(name: impl Into<String>) -> Self {
        Self {
            phase: Some(TaskProgressPhase::start(name)),
            ..Self::default()
        }
    }

    pub fn with_counter(
        mut self,
        name: impl Into<String>,
        completed: u64,
        total: Option<u64>,
    ) -> Self {
        self.set_counter(name, completed, total);
        self
    }

    pub fn set_counter(&mut self, name: impl Into<String>, completed: u64, total: Option<u64>) {
        let name = name.into();
        if let Some(counter) = self
            .counters
            .iter_mut()
            .find(|counter| counter.name == name)
        {
            counter.completed = completed;
            counter.total = total;
            return;
        }
        self.counters.push(TaskProgressCounter {
            name,
            completed,
            total,
        });
    }

    pub fn with_endpoint(mut self, endpoint: TaskEndpointSummary) -> Self {
        self.set_endpoint(endpoint);
        self
    }

    pub fn set_endpoint(&mut self, endpoint: TaskEndpointSummary) {
        if let Some(existing) = self
            .endpoints
            .iter_mut()
            .find(|existing| existing.name == endpoint.name)
        {
            *existing = endpoint;
            return;
        }
        self.endpoints.push(endpoint);
    }

    pub fn with_active_worker_kind(mut self, worker_kind: impl Into<String>) -> Self {
        self.active_worker_kind = Some(worker_kind.into());
        self
    }

    pub fn with_recent_status(mut self, status: impl Into<String>) -> Self {
        self.recent_status = Some(status.into());
        self
    }

    pub fn with_queue(
        mut self,
        position: usize,
        active_worker_kind: Option<String>,
        blocking_reason: Option<String>,
    ) -> Self {
        self.queue = Some(TaskQueueProgress {
            position,
            active_worker_kind,
            blocking_reason,
        });
        self
    }

    pub fn bounded(mut self) -> Self {
        if let Some(phase) = &mut self.phase {
            phase.name = bounded_chars(&phase.name, TASK_STRING_MAX_CHARS);
            phase.started_at = bounded_chars(&phase.started_at, TASK_STRING_MAX_CHARS);
        }
        self.counters.truncate(TASK_ARRAY_MAX_ITEMS);
        for counter in &mut self.counters {
            counter.name = bounded_chars(&counter.name, TASK_STRING_MAX_CHARS);
        }
        self.endpoints.truncate(TASK_ARRAY_MAX_ITEMS);
        for endpoint in &mut self.endpoints {
            endpoint.name = bounded_chars(&endpoint.name, TASK_STRING_MAX_CHARS);
            endpoint.latest_error = endpoint
                .latest_error
                .as_deref()
                .map(|error| bounded_chars(error, TASK_EVENT_MESSAGE_MAX_CHARS));
        }
        if let Some(queue) = &mut self.queue {
            queue.active_worker_kind = queue
                .active_worker_kind
                .as_deref()
                .map(|worker| bounded_chars(worker, TASK_STRING_MAX_CHARS));
            queue.blocking_reason = queue
                .blocking_reason
                .as_deref()
                .map(|reason| bounded_chars(reason, TASK_EVENT_MESSAGE_MAX_CHARS));
        }
        self.active_worker_kind = self
            .active_worker_kind
            .as_deref()
            .map(|worker| bounded_chars(worker, TASK_STRING_MAX_CHARS));
        self.recent_status = self
            .recent_status
            .as_deref()
            .map(|status| bounded_chars(status, TASK_EVENT_MESSAGE_MAX_CHARS));
        self
    }

    pub fn with_current_elapsed(mut self) -> Self {
        if let Some(phase) = &mut self.phase {
            if let Some(elapsed_ms) = elapsed_ms_since_unix_seconds(&phase.started_at) {
                phase.elapsed_ms = elapsed_ms;
            }
        }
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskProgressPhase {
    pub name: String,
    pub started_at: String,
    pub elapsed_ms: u64,
}

impl TaskProgressPhase {
    pub fn start(name: impl Into<String>) -> Self {
        Self {
            name: name.into(),
            started_at: unix_timestamp_string(),
            elapsed_ms: 0,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskProgressCounter {
    pub name: String,
    pub completed: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub total: Option<u64>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEndpointSummary {
    pub name: String,
    pub calls: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_token_latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p50_latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub p95_latency_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub latest_error: Option<String>,
}

impl TaskEndpointSummary {
    pub fn single_call(name: impl Into<String>, latency_ms: u64) -> Self {
        Self {
            name: name.into(),
            calls: 1,
            latest_latency_ms: Some(latency_ms),
            first_token_latency_ms: None,
            p50_latency_ms: Some(latency_ms),
            p95_latency_ms: Some(latency_ms),
            latest_error: None,
        }
    }

    pub fn failed_call(
        name: impl Into<String>,
        latency_ms: Option<u64>,
        error: impl Into<String>,
    ) -> Self {
        Self {
            name: name.into(),
            calls: 1,
            latest_latency_ms: latency_ms,
            first_token_latency_ms: None,
            p50_latency_ms: latency_ms,
            p95_latency_ms: latency_ms,
            latest_error: Some(error.into()),
        }
    }

    pub fn with_first_token_latency_ms(mut self, latency_ms: u64) -> Self {
        self.first_token_latency_ms = Some(latency_ms);
        self
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskQueueProgress {
    pub position: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub active_worker_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocking_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct PhaseTiming {
    phase: String,
    started_at: String,
    started: Instant,
}

impl PhaseTiming {
    pub fn start(phase: impl Into<String>) -> Self {
        Self {
            phase: phase.into(),
            started_at: unix_timestamp_string(),
            started: Instant::now(),
        }
    }

    pub fn progress_snapshot(&self) -> TaskProgressSnapshot {
        TaskProgressSnapshot {
            phase: Some(TaskProgressPhase {
                name: self.phase.clone(),
                started_at: self.started_at.clone(),
                elapsed_ms: self
                    .started
                    .elapsed()
                    .as_millis()
                    .try_into()
                    .unwrap_or(u64::MAX),
            }),
            ..TaskProgressSnapshot::default()
        }
    }

    pub fn finish(self, metadata: Value) -> FinishedPhaseTiming {
        FinishedPhaseTiming {
            phase: self.phase,
            started_at: self.started_at,
            duration_ms: self
                .started
                .elapsed()
                .as_millis()
                .try_into()
                .unwrap_or(u64::MAX),
            metadata: bounded_json(metadata),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FinishedPhaseTiming {
    pub phase: String,
    pub started_at: String,
    pub duration_ms: u64,
    pub metadata: Value,
}

pub fn unix_timestamp_string() -> String {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

fn unix_timestamp_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn elapsed_ms_since_unix_seconds(started_at: &str) -> Option<u64> {
    let started_ms = started_at.parse::<u128>().ok()?.saturating_mul(1000);
    unix_timestamp_millis()
        .saturating_sub(started_ms)
        .try_into()
        .ok()
}

pub fn ask_request_metadata(
    question: &str,
    source_id: Option<&str>,
    embedding_profile_id: Option<&str>,
    show_retrieval: bool,
    context_only: bool,
) -> Value {
    bounded_json(json!({
        "question_chars": question.chars().count(),
        "question_sha256": hex_sha256(question.as_bytes()),
        "source_id": source_id,
        "embedding_profile_id": embedding_profile_id,
        "show_retrieval": show_retrieval,
        "context_only": context_only,
    }))
}

pub fn retrieve_request_metadata(
    question: &str,
    source_id: Option<&str>,
    embedding_profile_id: Option<&str>,
    limit: usize,
    page_size: usize,
    page: usize,
) -> Value {
    bounded_json(json!({
        "question_chars": question.chars().count(),
        "question_sha256": hex_sha256(question.as_bytes()),
        "source_id": source_id,
        "embedding_profile_id": embedding_profile_id,
        "limit": limit,
        "page_size": page_size,
        "page": page,
    }))
}

pub fn ask_result_metadata(
    answer: &str,
    citation_count: usize,
    verified: bool,
    retrieval_included: bool,
) -> Value {
    bounded_json(json!({
        "answer_chars": answer.chars().count(),
        "answer_sha256": hex_sha256(answer.as_bytes()),
        "citation_count": citation_count,
        "verified": verified,
        "retrieval_included": retrieval_included,
    }))
}

pub fn retrieve_result_metadata(
    total_results: usize,
    returned_results: usize,
    rerank_enabled: bool,
) -> Value {
    bounded_json(json!({
        "total_results": total_results,
        "returned_results": returned_results,
        "rerank_enabled": rerank_enabled,
    }))
}

pub fn ingest_request_metadata(source_id: Option<&str>, force: bool) -> Value {
    ingest_task_request_metadata(source_id, force, None, false)
}

pub fn ingest_task_request_metadata(
    source_id: Option<&str>,
    force: bool,
    embedding_profile_id: Option<&str>,
    vectors_only: bool,
) -> Value {
    ingest_task_request_metadata_with_queue_claim(
        source_id,
        force,
        embedding_profile_id,
        vectors_only,
        false,
    )
}

pub fn ingest_task_request_metadata_with_queue_claim(
    source_id: Option<&str>,
    force: bool,
    embedding_profile_id: Option<&str>,
    vectors_only: bool,
    queue_claimable: bool,
) -> Value {
    ingest_task_request_metadata_with_queue_claim_and_batch(
        source_id,
        force,
        embedding_profile_id,
        vectors_only,
        queue_claimable,
        None,
    )
}

pub fn ingest_task_request_metadata_with_queue_claim_and_batch(
    source_id: Option<&str>,
    force: bool,
    embedding_profile_id: Option<&str>,
    vectors_only: bool,
    queue_claimable: bool,
    ingest_batch_id: Option<&str>,
) -> Value {
    bounded_json(json!({
        "ingest_request_version": 1,
        "source_id": source_id,
        "force": force,
        "embedding_profile_id": embedding_profile_id,
        "vectors_only": vectors_only,
        "queue_claimable": queue_claimable,
        "ingest_batch_id": ingest_batch_id,
    }))
}

pub fn reindex_task_request_metadata_with_queue_claim(
    source_id: Option<&str>,
    force: bool,
    embedding_profile_id: Option<&str>,
    vectors_only: bool,
    queue_claimable: bool,
) -> Value {
    reindex_task_request_metadata_with_queue_claim_and_batch(
        source_id,
        force,
        embedding_profile_id,
        vectors_only,
        queue_claimable,
        None,
    )
}

pub fn reindex_task_request_metadata_with_queue_claim_and_batch(
    source_id: Option<&str>,
    force: bool,
    embedding_profile_id: Option<&str>,
    vectors_only: bool,
    queue_claimable: bool,
    ingest_batch_id: Option<&str>,
) -> Value {
    let mut metadata = ingest_task_request_metadata_with_queue_claim_and_batch(
        source_id,
        force,
        embedding_profile_id,
        vectors_only,
        queue_claimable,
        ingest_batch_id,
    );
    if let Value::Object(map) = &mut metadata {
        map.insert(
            "operation".to_string(),
            Value::String("reindex".to_string()),
        );
    }
    bounded_json(metadata)
}

pub fn ingest_result_metadata(ingested: usize, embedding_cache: &EmbeddingCacheStats) -> Value {
    bounded_json(json!({
        "ingested": ingested,
        "embedding_cache": embedding_cache,
    }))
}

pub fn reindex_result_metadata(reindexed: usize, embedding_cache: &EmbeddingCacheStats) -> Value {
    bounded_json(json!({
        "reindexed": reindexed,
        "embedding_cache": embedding_cache,
    }))
}

pub fn bounded_json(value: Value) -> Value {
    let value = sanitize_value(value);
    let Ok(encoded) = serde_json::to_vec(&value) else {
        return json!({ "error": "metadata_not_serializable" });
    };
    if encoded.len() <= TASK_METADATA_MAX_BYTES {
        return value;
    }

    json!({
        "truncated": true,
        "original_bytes": encoded.len(),
        "sha256": hex_sha256(&encoded),
    })
}

pub fn bounded_message(message: &str) -> String {
    bounded_chars(message, TASK_EVENT_MESSAGE_MAX_CHARS)
}

pub fn bounded_error(message: &str) -> String {
    bounded_chars(message, TASK_ERROR_MAX_CHARS)
}

fn sanitize_value(value: Value) -> Value {
    match value {
        Value::Object(map) => sanitize_object(map),
        Value::Array(items) => Value::Array(
            items
                .into_iter()
                .take(TASK_ARRAY_MAX_ITEMS)
                .map(sanitize_value)
                .collect(),
        ),
        Value::String(text) => Value::String(bounded_chars(&text, TASK_STRING_MAX_CHARS)),
        other => other,
    }
}

fn sanitize_object(map: Map<String, Value>) -> Value {
    let mut output = Map::new();
    for (key, value) in map.into_iter().take(TASK_OBJECT_MAX_KEYS) {
        let sanitized = if is_sensitive_metadata_key(&key) {
            Value::String("<redacted>".into())
        } else if key == "response_body_prefix" {
            match value {
                Value::String(text) => {
                    Value::String(bounded_chars(&text, TASK_UPSTREAM_BODY_PREFIX_MAX_CHARS))
                }
                other => sanitize_value(other),
            }
        } else {
            sanitize_value(value)
        };
        output.insert(key, sanitized);
    }
    Value::Object(output)
}

fn is_sensitive_metadata_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace(['-', '_'], "");
    normalized.contains("apikey")
        || normalized.contains("token")
        || normalized.contains("secret")
        || normalized.contains("password")
        || normalized.contains("authorization")
        || normalized.contains("bearer")
        || matches!(
            normalized.as_str(),
            "prompt"
                | "rawprompt"
                | "rawresponse"
                | "modelresponse"
                | "response"
                | "answer"
                | "question"
                | "content"
                | "text"
        )
}

fn bounded_chars(input: &str, max_chars: usize) -> String {
    let mut output = String::new();
    for (idx, ch) in input.chars().enumerate() {
        if idx == max_chars {
            output.push_str("...[truncated]");
            return output;
        }
        output.push(ch);
    }
    output
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_metadata_does_not_store_raw_question_or_answer() {
        let question = "What is the secret launch password?";
        let request = ask_request_metadata(question, Some("src-1"), Some("default"), true, false);
        let retrieve =
            retrieve_request_metadata(question, Some("src-1"), Some("default"), 12, 1, 1);
        let result = ask_result_metadata("Raw answer text [E1].", 1, true, false);
        let encoded = serde_json::to_string(&(request, retrieve, result)).unwrap();

        assert!(encoded.contains("question_sha256"));
        assert!(encoded.contains("answer_sha256"));
        assert!(!encoded.contains(question));
        assert!(!encoded.contains("Raw answer text"));
    }

    #[test]
    fn bounded_json_redacts_sensitive_keys_and_caps_size() {
        let value = json!({
            "api_key": "should-not-print",
            "safe": "x".repeat(TASK_METADATA_MAX_BYTES * 2),
        });

        let bounded = bounded_json(value);
        let encoded = serde_json::to_string(&bounded).unwrap();

        assert!(!encoded.contains("should-not-print"));
        assert!(encoded.contains("<redacted>"));
        assert!(encoded.len() <= TASK_METADATA_MAX_BYTES);
    }

    #[test]
    fn bounded_json_preserves_upstream_body_prefix_budget() {
        let prefix = "x".repeat(1024);
        let bounded = bounded_json(json!({
            "upstream_failure": {
                "response_body_prefix": prefix,
                "response_body_bytes": 1024,
            }
        }));

        assert_eq!(
            bounded["upstream_failure"]["response_body_prefix"]
                .as_str()
                .unwrap()
                .len(),
            1024
        );
    }

    #[test]
    fn progress_snapshot_is_typed_bounded_and_elapsed() {
        let snapshot = TaskProgressSnapshot::phase("embedding".repeat(200))
            .with_counter("vectors", 12, Some(20))
            .with_endpoint(TaskEndpointSummary::failed_call(
                "embedding",
                Some(42),
                "remote timeout".repeat(100),
            ))
            .with_active_worker_kind("ingest")
            .with_recent_status("embedding batch");

        let bounded = snapshot.bounded().with_current_elapsed();
        let encoded = serde_json::to_string(&bounded).unwrap();

        assert!(encoded.contains("\"vectors\""));
        assert!(encoded.contains("\"latest_error\""));
        assert!(encoded.len() <= TASK_METADATA_MAX_BYTES);
        assert!(bounded.phase.unwrap().elapsed_ms < 5_000);
    }

    #[test]
    fn reindex_result_metadata_includes_embedding_cache_stats() {
        let stats = EmbeddingCacheStats {
            cache_hits: 2,
            cache_misses: 1,
            embedded_chunks: 1,
            reused_chunks: 2,
            changed_chunks: 1,
        };

        let result = reindex_result_metadata(1, &stats);

        assert_eq!(result["reindexed"], 1);
        assert_eq!(result["embedding_cache"]["cache_hits"], 2);
        assert_eq!(result["embedding_cache"]["cache_misses"], 1);
        assert_eq!(result["embedding_cache"]["embedded_chunks"], 1);
        assert_eq!(result["embedding_cache"]["reused_chunks"], 2);
        assert_eq!(result["embedding_cache"]["changed_chunks"], 1);
    }

    #[test]
    fn ingest_request_metadata_can_persist_batch_id() {
        let request = ingest_task_request_metadata_with_queue_claim_and_batch(
            Some("src-1"),
            false,
            None,
            false,
            true,
            Some("task-batch"),
        );

        assert_eq!(request["ingest_batch_id"], "task-batch");
        assert_eq!(request["queue_claimable"], true);
    }
}
