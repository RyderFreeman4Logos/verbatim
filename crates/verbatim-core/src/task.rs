//! Persistent task metadata shared by daemon, CLI, and storage code.
//!
//! Task records intentionally store bounded execution metadata, not raw user
//! prompts or raw model responses. Callers that need full ask answers must use
//! the synchronous/streaming API response path.

use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};
use serde_json::{json, Map, Value};

use crate::config::RerankStrategy;
use crate::resource::{ResourceQueueSnapshot, TaskResourceProgress};
use crate::types::{
    hex_sha256, EmbeddingCacheStats, RetrievalDenseVectorPath, VectorIndexResidency,
};

mod retrieve_profile;

pub use retrieve_profile::{
    RetrieveDenseStageProfile, RetrieveDisplayStageProfile, RetrieveEvidenceStageProfile,
    RetrieveRerankStageProfile, RetrieveStageProfile, RetrieveTaskProfile,
};

static TASK_ID_COUNTER: AtomicU64 = AtomicU64::new(1);

pub const TASK_METADATA_MAX_BYTES: usize = 8192;
pub const TASK_EVENT_MESSAGE_MAX_CHARS: usize = 512;
pub const TASK_ERROR_MAX_CHARS: usize = 2048;
pub const TASK_SPAN_MAX_PER_TASK: usize = 256;
pub const TASK_PROFILE_SCHEMA_VERSION: u32 = 1;
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

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IngestTaskStage {
    Ingest,
    Parse,
    Ocr,
    ImageCaption,
    ContextualRetrieval,
    GraphExpansion,
    Chunk,
    EmbeddingQueueWait,
    EmbeddingRequest,
    EmbeddingPostprocess,
    SqliteWrite,
    Bm25Index,
    VectorIndex,
    QdrantSync,
    TaskTerminalize,
    IngestCancelled,
}

impl IngestTaskStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ingest => "ingest",
            Self::Parse => "parse",
            Self::Ocr => "ocr",
            Self::ImageCaption => "image_caption",
            Self::ContextualRetrieval => "contextual_retrieval",
            Self::GraphExpansion => "graph_expansion",
            Self::Chunk => "chunk",
            Self::EmbeddingQueueWait => "embedding_queue_wait",
            Self::EmbeddingRequest => "embedding_request",
            Self::EmbeddingPostprocess => "embedding_postprocess",
            Self::SqliteWrite => "sqlite_write",
            Self::Bm25Index => "bm25_index",
            Self::VectorIndex => "vector_index",
            Self::QdrantSync => "qdrant_sync",
            Self::TaskTerminalize => "task_terminalize",
            Self::IngestCancelled => "ingest_cancelled",
        }
    }
}

pub const INGEST_TASK_STAGE_NAMES: &[&str] = &[
    IngestTaskStage::Ingest.as_str(),
    IngestTaskStage::Parse.as_str(),
    IngestTaskStage::Ocr.as_str(),
    IngestTaskStage::ImageCaption.as_str(),
    IngestTaskStage::ContextualRetrieval.as_str(),
    IngestTaskStage::GraphExpansion.as_str(),
    IngestTaskStage::Chunk.as_str(),
    IngestTaskStage::EmbeddingQueueWait.as_str(),
    IngestTaskStage::EmbeddingRequest.as_str(),
    IngestTaskStage::EmbeddingPostprocess.as_str(),
    IngestTaskStage::SqliteWrite.as_str(),
    IngestTaskStage::Bm25Index.as_str(),
    IngestTaskStage::VectorIndex.as_str(),
    IngestTaskStage::QdrantSync.as_str(),
    IngestTaskStage::TaskTerminalize.as_str(),
    IngestTaskStage::IngestCancelled.as_str(),
];

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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskProfile {
    pub schema_version: u32,
    pub task_id: TaskId,
    pub task_kind: TaskKind,
    pub status: TaskStatus,
    pub queue_wait_ms: u64,
    pub total_wall_ms: u64,
    #[serde(default)]
    pub controls: TaskProfileControls,
    #[serde(default)]
    pub resources: TaskResourceProfile,
    #[serde(default)]
    pub endpoints: Vec<TaskEndpointSummary>,
    #[serde(default)]
    pub retrieve: Option<RetrieveTaskProfile>,
    #[serde(default)]
    pub ask: Option<AskTaskProfile>,
}

/// Bounded execution controls captured when the task ran.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskProfileControls {
    pub retrieval: TaskRetrievalControls,
    pub rerank: TaskRerankControls,
    pub qdrant: TaskQdrantControls,
    pub vector: TaskVectorControls,
    pub filters: TaskFilterControls,
    pub output: TaskOutputControls,
}

/// Effective dense and lexical retrieval controls.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRetrievalControls {
    pub dense_top_k: Option<usize>,
    pub bm25_top_k: Option<usize>,
    pub rrf_k: Option<usize>,
    pub fast: bool,
    pub bypass_cache: bool,
}

/// Effective reranker controls and model role summary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskRerankControls {
    pub enabled: bool,
    pub configured_top_n: Option<usize>,
    pub effective_top_n: Option<usize>,
    pub strategy: Option<RerankStrategy>,
    pub provider: Option<String>,
    pub model: Option<String>,
}

/// Effective Qdrant search controls.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskQdrantControls {
    pub enabled: bool,
    pub preferred: bool,
    pub used: bool,
}

/// Effective embedding profile and dense vector path context.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskVectorControls {
    pub embedding_enabled: bool,
    pub embedding_profile_id: Option<String>,
    pub residency: Option<VectorIndexResidency>,
    pub dense_path: Option<RetrievalDenseVectorPath>,
}

/// Bounded source and collection filter summary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskFilterControls {
    pub source: TaskSourceFilterControls,
    pub collection: TaskCollectionFilterControls,
}

/// Bounded source filter summary.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskSourceFilterControls {
    pub requested_source_id: Option<String>,
    pub effective_source_count: Option<usize>,
}

/// Bounded collection filter summary; never includes member lists or paths.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCollectionFilterControls {
    pub requested_count: usize,
    pub requested_names: Vec<String>,
    pub requested_truncated: bool,
    pub require_fresh: bool,
    pub applied_count: Option<usize>,
    pub union_source_count: Option<usize>,
    pub stale: Option<bool>,
    pub warning_count: Option<usize>,
}

/// Effective output and pagination controls.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskOutputControls {
    pub limit: Option<usize>,
    pub page_size: Option<usize>,
    pub page: Option<usize>,
    pub passage: bool,
    pub include_locator: bool,
    pub include_debug: bool,
    pub include_debug_packs: bool,
    pub show_retrieval: Option<bool>,
}

/// Bounded resource queue summary captured when the task completed.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResourceProfile {
    pub queues: Vec<TaskResourceQueueSummary>,
}

/// Low-cardinality wait and service timing for one resource queue.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskResourceQueueSummary {
    pub name: String,
    pub kind: String,
    pub capacity: usize,
    pub queue_capacity: usize,
    pub queued: usize,
    pub active: usize,
    pub completed: u64,
    pub errors: u64,
    pub queue_wait_ms_total: u64,
    pub service_ms_total: u64,
    pub latest_queue_wait_ms: Option<u64>,
    pub latest_service_ms: Option<u64>,
}

impl From<&ResourceQueueSnapshot> for TaskResourceQueueSummary {
    fn from(snapshot: &ResourceQueueSnapshot) -> Self {
        Self {
            name: snapshot.name.clone(),
            kind: snapshot.kind.clone(),
            capacity: snapshot.capacity,
            queue_capacity: snapshot.queue_capacity,
            queued: snapshot.queued,
            active: snapshot.active,
            completed: snapshot.completed,
            errors: snapshot.errors,
            queue_wait_ms_total: snapshot.queue_wait_ms_total,
            service_ms_total: snapshot.service_ms_total,
            latest_queue_wait_ms: snapshot.last_queue_wait_ms,
            latest_service_ms: snapshot.last_service_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskTaskProfile {
    pub generation: AskGenerationStageProfile,
    pub verification: AskVerificationStageProfile,
    pub output: AskOutputStageProfile,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskGenerationStatus {
    Succeeded,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskGenerationStageProfile {
    pub status: AskGenerationStatus,
    pub call_count: u64,
    pub total_latency_ms: u64,
    pub latest_latency_ms: Option<u64>,
    pub retry_count: u64,
    pub error_count: u64,
    pub latest_error: Option<String>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AskVerificationStatus {
    Disabled,
    Passed,
    Revised,
    Failed,
    Skipped,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskVerificationStageProfile {
    pub enabled: bool,
    pub status: AskVerificationStatus,
    pub call_count: u64,
    pub total_latency_ms: u64,
    pub latest_latency_ms: Option<u64>,
    pub retry_count: u64,
    pub error_count: u64,
    pub latest_error: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskOutputStageProfile {
    pub response_formatting_ms: u64,
    pub answer_chars: usize,
    pub citation_count: usize,
    pub retrieval_included: bool,
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
    pub wait_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recent_status: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<TaskResourceProgress>,
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

    pub fn with_wait_reason(mut self, reason: impl Into<String>) -> Self {
        self.wait_reason = Some(reason.into());
        self
    }

    pub fn with_recent_status(mut self, status: impl Into<String>) -> Self {
        self.recent_status = Some(status.into());
        self
    }

    pub fn with_resource(mut self, resource: TaskResourceProgress) -> Self {
        self.set_resource(resource);
        self
    }

    pub fn set_resource(&mut self, resource: TaskResourceProgress) {
        if let Some(existing) = self
            .resources
            .iter_mut()
            .find(|existing| existing.name == resource.name)
        {
            *existing = resource;
            return;
        }
        self.resources.push(resource);
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
        self.wait_reason = self
            .wait_reason
            .as_deref()
            .map(|reason| bounded_chars(reason, TASK_STRING_MAX_CHARS));
        self.recent_status = self
            .recent_status
            .as_deref()
            .map(|status| bounded_chars(status, TASK_EVENT_MESSAGE_MAX_CHARS));
        self.resources.truncate(TASK_ARRAY_MAX_ITEMS);
        for resource in &mut self.resources {
            resource.name = bounded_chars(&resource.name, TASK_STRING_MAX_CHARS);
            resource.kind = bounded_chars(&resource.kind, TASK_STRING_MAX_CHARS);
            resource.state = bounded_chars(&resource.state, TASK_STRING_MAX_CHARS);
        }
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
    ingest_result_metadata_with_skips(ingested, embedding_cache, 0)
}

pub fn ingest_result_metadata_with_skips(
    ingested: usize,
    embedding_cache: &EmbeddingCacheStats,
    skipped_missing_sources: usize,
) -> Value {
    bounded_json(json!({
        "ingested": ingested,
        "skipped_missing_sources": skipped_missing_sources,
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
        let sanitized = if key == "canonical_target_tokens" && value.is_number() {
            value
        } else if is_sensitive_metadata_key(&key) {
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
    use crate::types::RetrievalRerankStatus;

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
    fn bounded_json_only_exposes_numeric_canonical_target_tokens() {
        assert_eq!(
            bounded_json(json!({ "canonical_target_tokens": 300 }))["canonical_target_tokens"],
            300
        );
        assert_eq!(
            bounded_json(json!({ "canonical_target_tokens": "secret" }))["canonical_target_tokens"],
            "<redacted>"
        );
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
    fn retrieve_task_profile_json_is_bounded_and_stage_oriented() {
        let profile = TaskProfile {
            schema_version: TASK_PROFILE_SCHEMA_VERSION,
            task_id: TaskId("task-slow-local".into()),
            task_kind: TaskKind::Retrieve,
            status: TaskStatus::Succeeded,
            queue_wait_ms: 5,
            total_wall_ms: 136_572,
            controls: TaskProfileControls {
                retrieval: TaskRetrievalControls {
                    dense_top_k: Some(80),
                    bm25_top_k: Some(50),
                    rrf_k: Some(60),
                    fast: false,
                    bypass_cache: false,
                },
                rerank: TaskRerankControls {
                    enabled: true,
                    configured_top_n: Some(20),
                    effective_top_n: Some(20),
                    strategy: Some(RerankStrategy::Endpoint),
                    provider: Some("vllm".into()),
                    model: Some("Qwen/Qwen3-Reranker-4B".into()),
                },
                qdrant: TaskQdrantControls {
                    enabled: true,
                    preferred: true,
                    used: false,
                },
                vector: TaskVectorControls {
                    embedding_enabled: true,
                    embedding_profile_id: Some("default".into()),
                    residency: Some(VectorIndexResidency::LowMemory),
                    dense_path: Some(RetrievalDenseVectorPath::LowMemorySqliteScan),
                },
                filters: TaskFilterControls {
                    source: TaskSourceFilterControls {
                        requested_source_id: Some("src-1".into()),
                        effective_source_count: Some(1),
                    },
                    collection: TaskCollectionFilterControls {
                        requested_count: 2,
                        requested_names: vec!["papers".into(), "notes".into()],
                        requested_truncated: false,
                        require_fresh: true,
                        applied_count: Some(2),
                        union_source_count: Some(30),
                        stale: Some(false),
                        warning_count: Some(0),
                    },
                },
                output: TaskOutputControls {
                    limit: Some(100),
                    page_size: Some(10),
                    page: Some(1),
                    passage: false,
                    include_locator: false,
                    include_debug: false,
                    include_debug_packs: false,
                    show_retrieval: None,
                },
            },
            resources: TaskResourceProfile {
                queues: vec![
                    TaskResourceQueueSummary {
                        name: "sqlite_reader".into(),
                        kind: "sqlite_read".into(),
                        capacity: 4,
                        queue_capacity: 16,
                        queued: 0,
                        active: 0,
                        completed: 1,
                        errors: 0,
                        queue_wait_ms_total: 2,
                        service_ms_total: 95,
                        latest_queue_wait_ms: Some(2),
                        latest_service_ms: Some(95),
                    },
                    TaskResourceQueueSummary {
                        name: "cpu_worker".into(),
                        kind: "cpu".into(),
                        capacity: 2,
                        queue_capacity: 16,
                        queued: 0,
                        active: 0,
                        completed: 0,
                        errors: 0,
                        queue_wait_ms_total: 0,
                        service_ms_total: 0,
                        latest_queue_wait_ms: None,
                        latest_service_ms: None,
                    },
                ],
            },
            endpoints: vec![
                TaskEndpointSummary::single_call("embedding", 761),
                TaskEndpointSummary::single_call("reranker", 1_357),
            ],
            retrieve: Some(RetrieveTaskProfile {
                candidate_counters: Default::default(),
                dense: RetrieveDenseStageProfile {
                    path: RetrievalDenseVectorPath::LowMemorySqliteScan,
                    candidate_count: 20_000,
                    local_ms: 96_000,
                    query_embedding_ms: 761,
                    endpoint_latency_ms: Some(761),
                },
                bm25: RetrieveStageProfile {
                    candidate_count: 5_000,
                    local_ms: 2_000,
                },
                fusion: RetrieveStageProfile {
                    candidate_count: 22_000,
                    local_ms: 900,
                },
                rerank: RetrieveRerankStageProfile {
                    status: RetrievalRerankStatus::Succeeded,
                    reason: None,
                    input_count: Some(100),
                    configured_top_n: 20,
                    effective_top_n: Some(20),
                    output_count: 20,
                    local_ms: 1_400,
                    endpoint_latency_ms: Some(1_357),
                },
                evidence: RetrieveEvidenceStageProfile {
                    result_count: 100,
                    graph_expanded_count: 4,
                    final_count: 1_500,
                    display_count: 10,
                    result_hydration_ms: 21_000,
                    graph_expansion_ms: 3_000,
                    final_pack_ms: 0,
                    display_pack_ms: 9_500,
                },
                display: RetrieveDisplayStageProfile {
                    returned_count: 10,
                    response_formatting_ms: 315,
                    canonical_support_embedding_ms: Some(120),
                    canonical_display_selection_ms: Some(250),
                    canonical_selected_count: Some(10),
                },
            }),
            ask: None,
        };

        let encoded = serde_json::to_string(&profile).unwrap();
        let value: Value = serde_json::from_str(&encoded).unwrap();

        assert_eq!(value["retrieve"]["dense"]["path"], "low_memory_sqlite_scan");
        assert_eq!(value["controls"]["retrieval"]["dense_top_k"], 80);
        assert_eq!(value["controls"]["retrieval"]["bm25_top_k"], 50);
        assert_eq!(value["controls"]["rerank"]["enabled"], true);
        assert_eq!(value["controls"]["rerank"]["configured_top_n"], 20);
        assert_eq!(value["controls"]["rerank"]["effective_top_n"], 20);
        assert_eq!(value["controls"]["rerank"]["strategy"], "endpoint");
        assert_eq!(value["controls"]["qdrant"]["enabled"], true);
        assert_eq!(value["controls"]["qdrant"]["preferred"], true);
        assert_eq!(value["controls"]["qdrant"]["used"], false);
        assert_eq!(value["controls"]["vector"]["residency"], "low_memory");
        assert_eq!(
            value["controls"]["vector"]["dense_path"],
            "low_memory_sqlite_scan"
        );
        assert_eq!(
            value["controls"]["filters"]["source"]["effective_source_count"],
            1
        );
        assert_eq!(
            value["controls"]["filters"]["collection"]["requested_names"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(value["controls"]["output"]["page_size"], 10);
        assert_eq!(value["resources"]["queues"][0]["name"], "sqlite_reader");
        assert_eq!(
            value["resources"]["queues"][0]["latest_service_ms"],
            json!(95)
        );
        assert_eq!(
            value["resources"]["queues"][1]["latest_service_ms"],
            Value::Null
        );
        assert_eq!(value["retrieve"]["dense"]["local_ms"], 96_000);
        assert_eq!(
            value["retrieve"]["dense"]["endpoint_latency_ms"],
            json!(761)
        );
        assert_eq!(value["retrieve"]["bm25"]["candidate_count"], 5_000);
        assert_eq!(value["retrieve"]["fusion"]["candidate_count"], 22_000);
        assert_eq!(value["retrieve"]["rerank"]["input_count"], 100);
        assert_eq!(value["retrieve"]["rerank"]["configured_top_n"], 20);
        assert_eq!(value["retrieve"]["rerank"]["effective_top_n"], 20);
        assert_eq!(
            value["retrieve"]["rerank"]["endpoint_latency_ms"],
            json!(1_357)
        );
        assert_eq!(value["retrieve"]["evidence"]["final_count"], 1_500);
        assert_eq!(value["retrieve"]["display"]["canonical_selected_count"], 10);
        assert_eq!(value["endpoints"][0]["name"], "embedding");
        assert_eq!(value["endpoints"][1]["name"], "reranker");
        assert!(encoded.len() <= TASK_METADATA_MAX_BYTES);
        assert!(!encoded.contains("chunk-"));
        assert!(!encoded.contains("bm25_hits"));
        assert!(!encoded.contains("dense_hits"));
        assert!(!encoded.contains("final_evidence_pack"));
        assert!(!encoded.contains("evidence_id"));
        assert!(!encoded.contains("prompt"));
        assert!(!encoded.contains("document"));
        assert!(!encoded.contains("api_key"));
    }

    #[test]
    fn legacy_minimal_task_profile_json_defaults_new_fields() {
        let profile: TaskProfile = serde_json::from_value(json!({
            "schema_version": TASK_PROFILE_SCHEMA_VERSION,
            "task_id": "task-legacy",
            "task_kind": "retrieve",
            "status": "succeeded",
            "queue_wait_ms": 0,
            "total_wall_ms": 17
        }))
        .unwrap();

        assert!(profile.endpoints.is_empty());
        assert_eq!(profile.controls, TaskProfileControls::default());
        assert_eq!(profile.resources, TaskResourceProfile::default());
        assert!(profile.retrieve.is_none());
        assert!(profile.ask.is_none());
    }

    #[test]
    fn ask_task_profile_json_is_bounded_and_stage_oriented() {
        let profile = TaskProfile {
            schema_version: TASK_PROFILE_SCHEMA_VERSION,
            task_id: TaskId("task-ask".into()),
            task_kind: TaskKind::Ask,
            status: TaskStatus::Succeeded,
            queue_wait_ms: 3,
            total_wall_ms: 2_050,
            controls: TaskProfileControls {
                retrieval: TaskRetrievalControls {
                    dense_top_k: Some(0),
                    bm25_top_k: Some(4),
                    rrf_k: Some(60),
                    fast: false,
                    bypass_cache: false,
                },
                rerank: TaskRerankControls {
                    enabled: false,
                    configured_top_n: Some(0),
                    effective_top_n: None,
                    strategy: Some(RerankStrategy::Endpoint),
                    provider: Some("vllm".into()),
                    model: Some("Qwen/Qwen3-Reranker-4B".into()),
                },
                qdrant: TaskQdrantControls::default(),
                vector: TaskVectorControls {
                    embedding_enabled: false,
                    embedding_profile_id: Some("default".into()),
                    residency: Some(VectorIndexResidency::LowMemory),
                    dense_path: Some(RetrievalDenseVectorPath::Bm25Only),
                },
                filters: TaskFilterControls::default(),
                output: TaskOutputControls {
                    limit: None,
                    page_size: None,
                    page: None,
                    passage: false,
                    include_locator: false,
                    include_debug: false,
                    include_debug_packs: false,
                    show_retrieval: Some(false),
                },
            },
            resources: TaskResourceProfile::default(),
            endpoints: vec![
                TaskEndpointSummary::single_call("chat", 800),
                TaskEndpointSummary {
                    name: "verifier".into(),
                    calls: 2,
                    latest_latency_ms: Some(300),
                    first_token_latency_ms: None,
                    p50_latency_ms: Some(250),
                    p95_latency_ms: Some(300),
                    latest_error: None,
                },
            ],
            retrieve: Some(RetrieveTaskProfile {
                candidate_counters: Default::default(),
                dense: RetrieveDenseStageProfile {
                    path: RetrievalDenseVectorPath::Bm25Only,
                    candidate_count: 0,
                    local_ms: 0,
                    query_embedding_ms: 0,
                    endpoint_latency_ms: None,
                },
                bm25: RetrieveStageProfile {
                    candidate_count: 4,
                    local_ms: 12,
                },
                fusion: RetrieveStageProfile {
                    candidate_count: 4,
                    local_ms: 1,
                },
                rerank: RetrieveRerankStageProfile {
                    status: RetrievalRerankStatus::Disabled,
                    reason: None,
                    input_count: None,
                    configured_top_n: 0,
                    effective_top_n: None,
                    output_count: 0,
                    local_ms: 0,
                    endpoint_latency_ms: None,
                },
                evidence: RetrieveEvidenceStageProfile {
                    result_count: 3,
                    graph_expanded_count: 0,
                    final_count: 3,
                    display_count: 3,
                    result_hydration_ms: 2,
                    graph_expansion_ms: 0,
                    final_pack_ms: 1,
                    display_pack_ms: 1,
                },
                display: RetrieveDisplayStageProfile {
                    returned_count: 3,
                    response_formatting_ms: 0,
                    canonical_support_embedding_ms: None,
                    canonical_display_selection_ms: None,
                    canonical_selected_count: None,
                },
            }),
            ask: Some(AskTaskProfile {
                generation: AskGenerationStageProfile {
                    status: AskGenerationStatus::Succeeded,
                    call_count: 1,
                    total_latency_ms: 800,
                    latest_latency_ms: Some(800),
                    retry_count: 0,
                    error_count: 0,
                    latest_error: None,
                },
                verification: AskVerificationStageProfile {
                    enabled: true,
                    status: AskVerificationStatus::Revised,
                    call_count: 2,
                    total_latency_ms: 550,
                    latest_latency_ms: Some(300),
                    retry_count: 0,
                    error_count: 0,
                    latest_error: None,
                },
                output: AskOutputStageProfile {
                    response_formatting_ms: 7,
                    answer_chars: 42,
                    citation_count: 1,
                    retrieval_included: false,
                },
            }),
        };

        let encoded = serde_json::to_string(&profile).unwrap();
        let value: Value = serde_json::from_str(&encoded).unwrap();

        assert_eq!(value["task_kind"], "ask");
        assert_eq!(value["controls"]["retrieval"]["bm25_top_k"], 4);
        assert_eq!(value["controls"]["rerank"]["enabled"], false);
        assert_eq!(value["controls"]["rerank"]["effective_top_n"], Value::Null);
        assert_eq!(value["controls"]["qdrant"]["enabled"], false);
        assert_eq!(value["controls"]["output"]["show_retrieval"], false);
        assert_eq!(value["resources"]["queues"].as_array().unwrap().len(), 0);
        assert_eq!(value["retrieve"]["bm25"]["candidate_count"], 4);
        assert_eq!(value["ask"]["generation"]["call_count"], 1);
        assert_eq!(value["ask"]["generation"]["latest_latency_ms"], 800);
        assert_eq!(value["ask"]["generation"]["retry_count"], 0);
        assert_eq!(value["ask"]["verification"]["enabled"], true);
        assert_eq!(value["ask"]["verification"]["status"], "revised");
        assert_eq!(value["ask"]["verification"]["call_count"], 2);
        assert_eq!(value["ask"]["output"]["response_formatting_ms"], 7);
        assert_eq!(value["ask"]["output"]["citation_count"], 1);
        assert!(encoded.len() <= TASK_METADATA_MAX_BYTES);
        assert!(!encoded.contains("SOURCE PACK"));
        assert!(!encoded.contains("USER QUESTION"));
        assert!(!encoded.contains("document body"));
        assert!(!encoded.contains("api_key"));
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
    fn ingest_stage_contract_contains_required_low_cardinality_names() {
        let required = [
            "parse",
            "chunk",
            "embedding_queue_wait",
            "embedding_request",
            "embedding_postprocess",
            "sqlite_write",
            "bm25_index",
            "vector_index",
            "qdrant_sync",
            "task_terminalize",
        ];

        for stage in required {
            assert!(
                INGEST_TASK_STAGE_NAMES.contains(&stage),
                "missing ingest task stage {stage}"
            );
        }
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
