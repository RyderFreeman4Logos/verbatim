//! Persistent task metadata shared by daemon, CLI, and storage code.
//!
//! Task records intentionally store bounded execution metadata, not raw user
//! prompts or raw model responses. Callers that need full ask answers must use
//! the synchronous/streaming API response path.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::types::hex_sha256;

static TASK_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

pub const TASK_METADATA_MAX_BYTES: usize = 2048;
pub const TASK_EVENT_MESSAGE_MAX_CHARS: usize = 512;
pub const TASK_ERROR_MAX_CHARS: usize = 2048;
const TASK_STRING_MAX_CHARS: usize = 256;
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
}

impl TaskKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Ask => "ask",
            Self::Ingest => "ingest",
        }
    }

    pub fn from_store_str(value: &str) -> Option<Self> {
        match value {
            "ask" => Some(Self::Ask),
            "ingest" => Some(Self::Ingest),
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

pub fn ask_request_metadata(
    question: &str,
    source_id: Option<&str>,
    show_retrieval: bool,
) -> Value {
    bounded_json(json!({
        "question_chars": question.chars().count(),
        "question_sha256": hex_sha256(question.as_bytes()),
        "source_id": source_id,
        "show_retrieval": show_retrieval,
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

pub fn ingest_request_metadata(source_id: Option<&str>, force: bool) -> Value {
    bounded_json(json!({
        "source_id": source_id,
        "force": force,
    }))
}

pub fn ingest_result_metadata(ingested: usize) -> Value {
    bounded_json(json!({ "ingested": ingested }))
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
        let request = ask_request_metadata(question, Some("src-1"), true);
        let result = ask_result_metadata("Raw answer text [E1].", 1, true, false);
        let encoded = serde_json::to_string(&(request, result)).unwrap();

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
}
