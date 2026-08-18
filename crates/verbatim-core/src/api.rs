//! HTTP API payloads shared by the daemon and thin CLI.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::collection::{
    CollectionMember, CollectionRecord, CollectionRoot, CollectionStatus, CollectionSyncReport,
};
use crate::config::ConfigReloadMetadata;
use crate::index_gc::{IndexGcApplyReport, IndexGcConfig, IndexGcPlan};
use crate::index_profile_delete::{IndexProfileDeleteApplyReport, IndexProfileDeletePlan};
use crate::memory_budget::MemoryBudgetSnapshot;
use crate::resource::ResourceQueueSnapshot;
use crate::store::VectorJsonCleanupReport;
use crate::task::{TaskEvent, TaskProfile, TaskSpan, TaskSummary};
use crate::types::{
    BBox, ImageArtifact, RetrievalDebug, RetrievalProvenance, SourceIngestDiagnostics,
    SourceLocator,
};

/// HTTP methods used by shared daemon API route metadata.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiHttpMethod {
    Delete,
    Get,
    Post,
    Put,
}

impl ApiHttpMethod {
    /// Returns the wire-format method name used in request lines and docs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Delete => "DELETE",
            Self::Get => "GET",
            Self::Post => "POST",
            Self::Put => "PUT",
        }
    }
}

/// Daemon endpoints that back collection-era CLI and GUI workflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CollectionApiEndpoint {
    CreateCollection,
    ListCollections,
    GetCollection,
    DeleteCollection,
    AddCollectionRoot,
    SyncCollection,
    CollectionStatus,
    ListCollectionWatcherStatuses,
    CollectionWatcherStatus,
    UpdateCollectionWatcher,
}

impl CollectionApiEndpoint {
    /// HTTP method for this endpoint.
    pub const fn method(self) -> ApiHttpMethod {
        match self {
            Self::CreateCollection | Self::AddCollectionRoot | Self::SyncCollection => {
                ApiHttpMethod::Post
            }
            Self::ListCollections
            | Self::GetCollection
            | Self::CollectionStatus
            | Self::ListCollectionWatcherStatuses
            | Self::CollectionWatcherStatus => ApiHttpMethod::Get,
            Self::DeleteCollection => ApiHttpMethod::Delete,
            Self::UpdateCollectionWatcher => ApiHttpMethod::Put,
        }
    }

    /// Axum-style route template registered by the daemon.
    pub const fn path_template(self) -> &'static str {
        match self {
            Self::CreateCollection | Self::ListCollections => "/api/collections",
            Self::GetCollection | Self::DeleteCollection => "/api/collections/{name}",
            Self::AddCollectionRoot => "/api/collections/{name}/roots",
            Self::SyncCollection => "/api/collections/{name}/sync",
            Self::CollectionStatus => "/api/collections/{name}/status",
            Self::ListCollectionWatcherStatuses => "/api/collections/watchers/status",
            Self::CollectionWatcherStatus | Self::UpdateCollectionWatcher => {
                "/api/collections/{name}/watcher"
            }
        }
    }

    /// Builds the client path for endpoints with a `{name}` placeholder.
    pub fn path(self, collection_name: &str) -> String {
        self.path_template().replace("{name}", collection_name)
    }
}

/// Mechanical mapping from a collection CLI leaf command to its daemon API.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CollectionCliApiParity {
    /// Canonical CLI command path without the binary name.
    pub cli_path: &'static str,
    /// Daemon endpoint that implements the command's stateful behavior.
    pub endpoint: CollectionApiEndpoint,
}

/// Canonical collection CLI/API parity inventory.
pub const COLLECTION_CLI_API_PARITY: &[CollectionCliApiParity] = &[
    CollectionCliApiParity {
        cli_path: "collection create",
        endpoint: CollectionApiEndpoint::CreateCollection,
    },
    CollectionCliApiParity {
        cli_path: "collection add-root",
        endpoint: CollectionApiEndpoint::AddCollectionRoot,
    },
    CollectionCliApiParity {
        cli_path: "collection list",
        endpoint: CollectionApiEndpoint::ListCollections,
    },
    CollectionCliApiParity {
        cli_path: "collection get",
        endpoint: CollectionApiEndpoint::GetCollection,
    },
    CollectionCliApiParity {
        cli_path: "collection delete",
        endpoint: CollectionApiEndpoint::DeleteCollection,
    },
    CollectionCliApiParity {
        cli_path: "collection sync",
        endpoint: CollectionApiEndpoint::SyncCollection,
    },
    CollectionCliApiParity {
        cli_path: "collection status",
        endpoint: CollectionApiEndpoint::CollectionStatus,
    },
    CollectionCliApiParity {
        cli_path: "collection watch enable",
        endpoint: CollectionApiEndpoint::UpdateCollectionWatcher,
    },
    CollectionCliApiParity {
        cli_path: "collection watch disable",
        endpoint: CollectionApiEndpoint::UpdateCollectionWatcher,
    },
    CollectionCliApiParity {
        cli_path: "collection watch status",
        endpoint: CollectionApiEndpoint::ListCollectionWatcherStatuses,
    },
    CollectionCliApiParity {
        cli_path: "collection watch status <name>",
        endpoint: CollectionApiEndpoint::CollectionWatcherStatus,
    },
];

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddSourceRequest {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddSourceResponse {
    pub id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelocateSourceRequest {
    pub source_id: String,
    pub new_path: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SourceResponse {
    pub id: String,
    pub path: String,
    pub status: String,
    pub hash: String,
    pub parser_used: Option<String>,
    pub last_ingested_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub diagnostics: Option<SourceIngestDiagnostics>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CheckStaleResponse {
    pub stale: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_status: Option<IndexStatusResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexStatusResponse {
    pub embedding_enabled: bool,
    pub active_profile_id: String,
    pub source_count: usize,
    pub stale_source_count: usize,
    pub stale_source_ids: Vec<String>,
    pub capability: EmbeddingCapabilityStatusResponse,
    pub chunking: ChunkingProfileStatusResponse,
    #[serde(default)]
    pub messages: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EmbeddingCapabilityStatusResponse {
    pub provider: String,
    pub model: String,
    pub dimension: usize,
    pub normalize: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub endpoint_identity: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub served_model: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_context_tokens: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dtype: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quantization: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub weight_identity: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChunkingProfileStatusResponse {
    pub version: String,
    pub child_target_tokens: usize,
    pub child_overlap_tokens: usize,
    pub parent_children_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_input_budget_tokens: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CreateCollectionRequest {
    pub name: String,
    #[serde(default)]
    pub ignore_patterns: Vec<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddCollectionRootRequest {
    pub path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AddCollectionRootResponse {
    pub collection_name: String,
    pub root: CollectionRoot,
    pub root_count: usize,
    pub member_count: usize,
    pub added: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionSyncPathRequest {
    pub path: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub logical_path: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionSyncRequest {
    #[serde(default)]
    pub paths: Vec<CollectionSyncPathRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_depth: Option<usize>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectionResponse {
    pub collection: CollectionRecord,
    #[serde(default)]
    pub roots: Vec<CollectionRoot>,
    #[serde(default)]
    pub members: Vec<CollectionMember>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CollectionStatusResponse {
    pub status: CollectionStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionWatcherUpdateRequest {
    pub enabled: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_index_enabled: Option<bool>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionWatcherResponse {
    pub collection: CollectionRecord,
    pub watcher: CollectionWatcherStatus,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionWatchersStatusResponse {
    #[serde(default)]
    pub watchers: Vec<CollectionWatcherStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionWatcherStatus {
    pub collection_name: String,
    pub watch_enabled: bool,
    pub auto_index_enabled: bool,
    pub active: bool,
    pub ignored_by_config: bool,
    pub watched_root_count: usize,
    pub pending_event_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_event_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_sync_at: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
    #[serde(default)]
    pub last_added: usize,
    #[serde(default)]
    pub last_removed: usize,
    #[serde(default)]
    pub last_unchanged: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_task_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionSyncResponse {
    pub report: CollectionSyncReport,
}

#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionFilterRequest {
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collection_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub names: Vec<String>,
    #[serde(default)]
    pub require_fresh: bool,
}

impl CollectionFilterRequest {
    pub fn is_empty(&self) -> bool {
        self.collection_ids.is_empty() && self.names.is_empty() && !self.require_fresh
    }

    pub fn has_filters(&self) -> bool {
        !self.collection_ids.is_empty() || !self.names.is_empty()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionFilterResponse {
    pub requested: CollectionFilterRequest,
    pub union_source_count: usize,
    #[serde(default)]
    pub applied: Vec<AppliedCollectionFilterResponse>,
    #[serde(default)]
    pub warnings: Vec<String>,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedCollectionFilterResponse {
    pub collection_id: String,
    pub name: String,
    pub member_count: usize,
    pub indexed_member_count: usize,
    pub stale_member_count: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_synced_at: Option<String>,
    pub stale: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionResultProvenance {
    pub collection_id: String,
    pub name: String,
    pub logical_path: String,
    pub source_path: String,
    pub member_updated_at: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IngestResponse {
    pub ingested: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReindexRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default)]
    pub all: bool,
    #[serde(default)]
    pub stale: bool,
    #[serde(default)]
    pub force: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_profile_id: Option<String>,
    #[serde(default)]
    pub vectors_only: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReindexResponse {
    pub reindexed: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexGcRequest {
    #[serde(default)]
    pub dry_run: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexGcResponse {
    pub dry_run: bool,
    pub policy: IndexGcConfig,
    pub plan: IndexGcPlan,
    pub apply: IndexGcApplyReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexProfileDeleteRequest {
    pub profile_id: String,
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub confirm: bool,
    #[serde(default)]
    pub allow_active: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexProfileDeleteResponse {
    pub dry_run: bool,
    pub plan: IndexProfileDeletePlan,
    pub apply: IndexProfileDeleteApplyReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorJsonCleanupRequest {
    #[serde(default)]
    pub dry_run: bool,
    #[serde(default)]
    pub confirm: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct VectorJsonCleanupResponse {
    pub dry_run: bool,
    pub report: VectorJsonCleanupReport,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskCreatedResponse {
    pub task_id: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskIngestRequest {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default)]
    pub force: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_profile_id: Option<String>,
    #[serde(default)]
    pub vectors_only: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskSummaryResponse {
    pub task: TaskSummary,
    #[serde(default)]
    pub spans: Vec<TaskSpan>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskProfileResponse {
    pub profile: TaskProfile,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskListResponse {
    #[serde(default)]
    pub tasks: Vec<TaskSummary>,
    pub total: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub aggregate: Option<TaskListAggregate>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskListAggregate {
    pub active_total: usize,
    pub active_sample_size: usize,
    pub active_sample_limit: usize,
    pub turnover: TaskQueueTurnover,
    pub embedding_wait: TaskEmbeddingWaitAggregate,
    pub stale_running: TaskStaleRunningAggregate,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskQueueTurnover {
    pub window: TaskQueueTurnoverWindow,
    pub recent_terminalized: usize,
    pub recent_succeeded: usize,
    pub recent_failed: usize,
    pub recent_cancelled: usize,
    pub recent_backfilled: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskQueueTurnoverWindow {
    pub event_sequence_floor: i64,
    pub event_sequence_ceiling: i64,
    pub event_limit: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskEmbeddingWaitAggregate {
    pub waiting: usize,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub oldest_wait_ms: Option<u64>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reason_buckets: Vec<TaskReasonBucket>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskStaleRunningAggregate {
    pub publish_complete_running: usize,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reason_buckets: Vec<TaskReasonBucket>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TaskReasonBucket {
    pub reason: String,
    pub count: usize,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskEventsResponse {
    #[serde(default)]
    pub events: Vec<TaskEvent>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskWaitEvent {
    pub task: TaskSummary,
    #[serde(default)]
    pub events: Vec<TaskEvent>,
    #[serde(default)]
    pub spans: Vec<TaskSpan>,
    pub terminal: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskRequest {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "CollectionFilterRequest::is_empty")]
    pub collection_filter: CollectionFilterRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_profile_id: Option<String>,
    #[serde(default)]
    pub show_retrieval: bool,
    #[serde(default)]
    pub context_only: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<usize>,
}

/// Model-authored text that must remain distinct from persisted evidence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GeneratedInterpretationResponse {
    pub text: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnswerKind {
    GeneratedInterpretation,
    EvidenceOnly,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskResponse {
    pub answer: String,
    pub answer_kind: AnswerKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generated_interpretation: Option<GeneratedInterpretationResponse>,
    #[serde(default)]
    pub citations: Vec<CitationResponse>,
    pub verified: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval: Option<RetrievalDebug>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context: Option<RetrieveResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_filter: Option<CollectionFilterResponse>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrieveRequest {
    pub question: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "CollectionFilterRequest::is_empty")]
    pub collection_filter: CollectionFilterRequest,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_profile_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub limit: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page_size: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<usize>,
    #[serde(default)]
    pub fast: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub dense_top_k: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bm25_top_k: Option<usize>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub rerank_top_n: Option<usize>,
    #[serde(default)]
    pub bypass_cache: bool,
    #[serde(default)]
    pub include_debug: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub include_debug_packs: bool,
    #[serde(default)]
    pub include_locator: bool,
    #[serde(default, skip_serializing_if = "is_false")]
    pub passage: bool,
}

fn is_false(value: &bool) -> bool {
    !*value
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrieveResponse {
    pub task_id: String,
    pub query: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_id: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub collection_filter: Option<CollectionFilterResponse>,
    pub embedding_profile_id: String,
    pub limit: usize,
    pub page_size: usize,
    pub page: usize,
    pub total_results: usize,
    pub returned_results: usize,
    pub source_bounded: bool,
    pub controls: RetrieveControlsResponse,
    pub audit_receipt: AuditReceipt,
    #[serde(default)]
    pub timings: Vec<RetrieveTimingResponse>,
    #[serde(default)]
    pub results: Vec<RetrieveResultResponse>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub debug: Option<RetrievalDebug>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrieveControlsResponse {
    pub fast: bool,
    pub rerank_enabled: bool,
    pub dense_top_k: usize,
    pub bm25_top_k: usize,
    pub rrf_k: usize,
    pub rerank_top_n: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrieveTimingResponse {
    pub phase: String,
    pub duration_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrieveResultResponse {
    pub index: usize,
    pub rank: usize,
    pub label: String,
    pub evidence_id: String,
    pub text_hash: String,
    pub source_id: String,
    pub source_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_path: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collections: Vec<CollectionResultProvenance>,
    pub chunk_id: String,
    pub kind: String,
    pub role: String,
    pub score: f32,
    pub locator: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub structured_locator: Option<SourceLocator>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provenance: Option<RetrievalProvenance>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub derived_from: Option<String>,
    pub snippet: String,
}

pub const AUDIT_RECEIPT_VERSION: u8 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditReceipt {
    pub version: u8,
    pub embedding_profile_id: String,
    pub source_bounded: bool,
    pub controls: RetrieveControlsResponse,
    pub results: Vec<AuditReceiptResult>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditReceiptResult {
    pub evidence_id: String,
    pub text_hash: String,
    pub source_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationResponse {
    pub label: String,
    pub evidence_id: String,
    pub kind: String,
    pub derived_from: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub collections: Vec<CollectionResultProvenance>,
    pub locator: String,
    pub text_preview: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EvidenceResponse {
    pub id: String,
    pub source_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_hash: Option<String>,
    pub source_bounded: bool,
    pub text_hash: String,
    pub kind: String,
    pub derived_from: Option<String>,
    pub locator: String,
    pub structured_locator: SourceLocator,
    pub text: String,
    pub heading_path: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub language: Option<String>,
    pub position: u32,
    pub image_artifact: Option<ImageArtifactResponse>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ImageArtifactResponse {
    pub image_id: String,
    pub path: String,
    pub content_hash: String,
    pub mime_type: String,
    pub width: u32,
    pub height: u32,
    pub page: u32,
    pub image_index: u32,
    pub bbox: Option<BBox>,
}

impl From<ImageArtifact> for ImageArtifactResponse {
    fn from(artifact: ImageArtifact) -> Self {
        Self {
            image_id: artifact.image_id.0,
            path: artifact.relative_path.display().to_string(),
            content_hash: artifact.content_hash,
            mime_type: artifact.mime_type,
            width: artifact.width,
            height: artifact.height,
            page: artifact.page,
            image_index: artifact.image_index,
            bbox: artifact.bbox,
        }
    }
}

/// Idle memory reclaim state exposed through daemon health and CLI status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleReclaimHealth {
    pub enabled: bool,
    pub sqlite_shrink_memory: bool,
    pub malloc_trim: bool,
    pub currently_idle: bool,
    pub eligible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    pub idle_for_millis: u64,
    pub idle_timeout_millis: u64,
    pub min_interval_millis: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_eligible_in_millis: Option<u64>,
    pub active: IdleReclaimActivitySnapshot,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_result: Option<IdleReclaimCycleResult>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_attempt_result: Option<IdleReclaimCycleResult>,
}

/// Low-cardinality counters used to explain why idle reclaim is or is not eligible.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleReclaimActivitySnapshot {
    pub http_requests: u64,
    pub sse_streams: u64,
    pub active_tasks: usize,
    pub resource_active: usize,
    pub resource_queued: usize,
    pub ingest_queue_active: bool,
    pub ingest_worker_active: bool,
    pub pipeline_busy: bool,
}

/// Last idle reclaim scheduler decision and per-backend outcomes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleReclaimCycleResult {
    pub attempted_at_unix_ms: u64,
    pub finished_at_unix_ms: u64,
    pub status: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    pub sqlite: IdleReclaimBackendResult,
    pub allocator: IdleReclaimBackendResult,
}

/// Best-effort result for one reclaim backend.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleReclaimBackendResult {
    pub status: String,
    pub attempted: bool,
    pub success_count: u64,
    pub failure_count: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_error: Option<String>,
}

impl IdleReclaimBackendResult {
    pub fn disabled() -> Self {
        Self {
            status: "disabled".into(),
            attempted: false,
            success_count: 0,
            failure_count: 0,
            last_error: None,
        }
    }

    pub fn skipped() -> Self {
        Self {
            status: "skipped".into(),
            attempted: false,
            success_count: 0,
            failure_count: 0,
            last_error: None,
        }
    }
}

/// Idle process exit state exposed through daemon health and CLI status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleExitHealth {
    pub enabled: bool,
    pub count_health_requests: bool,
    pub allow_with_collection_watcher: bool,
    pub auto_start_on_cli: bool,
    pub currently_idle: bool,
    pub eligible: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
    pub idle_for_millis: u64,
    pub timeout_millis: u64,
    pub last_activity_unix_ms: u64,
    pub deadline_unix_ms: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_eligible_in_millis: Option<u64>,
    pub active: IdleExitActivitySnapshot,
}

/// Low-cardinality counters used to explain idle exit blockers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct IdleExitActivitySnapshot {
    pub http_requests: u64,
    pub sse_streams: u64,
    pub active_tasks: usize,
    pub resource_active: usize,
    pub resource_queued: usize,
    pub ingest_queue_active: bool,
    pub ingest_worker_active: bool,
    pub pipeline_busy: bool,
    pub watched_roots: usize,
    pub pending_watcher_events: usize,
}

/// Process and retrieval readiness exposed through daemon health.
///
/// These fields are flattened into `HealthResponse` so scripts can read them
/// without knowing a nested object shape. Defaults intentionally treat older
/// daemon payloads as ready because they did not expose startup readiness.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReadinessHealth {
    #[serde(default = "default_process_alive")]
    pub process_alive: bool,
    #[serde(default = "default_readiness_state", rename = "readiness")]
    pub state: String,
    #[serde(default = "default_retrieval_ready")]
    pub retrieval_ready: bool,
    #[serde(default = "default_startup_phase")]
    pub startup_phase: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<String>,
}

impl Default for ReadinessHealth {
    fn default() -> Self {
        Self::ready()
    }
}

impl ReadinessHealth {
    pub fn ready() -> Self {
        Self {
            process_alive: true,
            state: "ready".into(),
            retrieval_ready: true,
            startup_phase: "ready".into(),
            degraded_reason: None,
        }
    }

    pub fn starting(phase: impl Into<String>, degraded_reason: Option<String>) -> Self {
        Self {
            process_alive: true,
            state: "starting".into(),
            retrieval_ready: false,
            startup_phase: phase.into(),
            degraded_reason,
        }
    }

    pub fn degraded(phase: impl Into<String>, degraded_reason: Option<String>) -> Self {
        Self {
            process_alive: true,
            state: "degraded".into(),
            retrieval_ready: false,
            startup_phase: phase.into(),
            degraded_reason,
        }
    }
}

fn default_process_alive() -> bool {
    true
}

fn default_readiness_state() -> String {
    "ready".into()
}

fn default_retrieval_ready() -> bool {
    true
}

fn default_startup_phase() -> String {
    "ready".into()
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
    #[serde(default, flatten)]
    pub readiness: ReadinessHealth,
    #[serde(default)]
    pub memory_budget: MemoryBudgetSnapshot,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub resources: Vec<ResourceQueueSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_reclaim: Option<IdleReclaimHealth>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub idle_exit: Option<IdleExitHealth>,
    /// Selected durability policy plus PRAGMAs and disk headroom as reported
    /// by the live SQLite task store.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub sqlite_durability: Option<crate::store::SqliteDurabilityStatus>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: Box<str>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub code: Option<Box<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub readiness: Option<Box<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_ready: Option<bool>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub startup_phase: Option<Box<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub degraded_reason: Option<Box<str>>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_failure: Option<Box<serde_json::Value>>,
}

impl ErrorResponse {
    pub fn new(error: impl Into<String>) -> Self {
        Self {
            error: error.into().into_boxed_str(),
            code: None,
            readiness: None,
            retrieval_ready: None,
            startup_phase: None,
            degraded_reason: None,
            upstream_failure: None,
        }
    }

    pub fn retrieval_not_ready(error: impl Into<String>, readiness: ReadinessHealth) -> Self {
        Self {
            error: error.into().into_boxed_str(),
            code: Some("retrieval_not_ready".into()),
            readiness: Some(readiness.state.into_boxed_str()),
            retrieval_ready: Some(readiness.retrieval_ready),
            startup_phase: Some(readiness.startup_phase.into_boxed_str()),
            degraded_reason: readiness.degraded_reason.map(String::into_boxed_str),
            upstream_failure: None,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskTokenEvent {
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskCitationEvent {
    #[serde(default)]
    pub citations: Vec<CitationResponse>,
    pub verified: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskErrorEvent {
    pub status: Option<u16>,
    pub error: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ConfigResponse {
    pub config: Value,
    pub reload: ConfigReloadMetadata,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ask_request_accepts_context_only_flag() {
        let request: AskRequest = serde_json::from_value(serde_json::json!({
            "question": "What is cited?",
            "context_only": true
        }))
        .unwrap();

        assert!(request.context_only);
        assert!(!request.show_retrieval);
    }

    include!("api_answer_kind_tests.rs");

    #[test]
    fn collection_cli_api_parity_inventory_is_daemon_backed() {
        let entries = COLLECTION_CLI_API_PARITY
            .iter()
            .map(|entry| {
                (
                    entry.cli_path,
                    entry.endpoint.method().as_str(),
                    entry.endpoint.path_template(),
                )
            })
            .collect::<Vec<_>>();

        assert_eq!(
            entries,
            vec![
                ("collection create", "POST", "/api/collections"),
                (
                    "collection add-root",
                    "POST",
                    "/api/collections/{name}/roots"
                ),
                ("collection list", "GET", "/api/collections"),
                ("collection get", "GET", "/api/collections/{name}"),
                ("collection delete", "DELETE", "/api/collections/{name}"),
                ("collection sync", "POST", "/api/collections/{name}/sync"),
                ("collection status", "GET", "/api/collections/{name}/status"),
                (
                    "collection watch enable",
                    "PUT",
                    "/api/collections/{name}/watcher"
                ),
                (
                    "collection watch disable",
                    "PUT",
                    "/api/collections/{name}/watcher"
                ),
                (
                    "collection watch status",
                    "GET",
                    "/api/collections/watchers/status"
                ),
                (
                    "collection watch status <name>",
                    "GET",
                    "/api/collections/{name}/watcher"
                ),
            ]
        );
    }

    #[test]
    fn ask_request_defaults_to_generated_answer_without_context_pagination() {
        let request: AskRequest =
            serde_json::from_value(serde_json::json!({"question": "What is cited?"})).unwrap();

        assert_eq!(request.question, "What is cited?");
        assert!(request.source_id.is_none());
        assert!(request.collection_filter.is_empty());
        assert!(request.embedding_profile_id.is_none());
        assert!(!request.show_retrieval);
        assert!(!request.context_only);
        assert!(request.limit.is_none());
        assert!(request.page_size.is_none());
        assert!(request.page.is_none());
    }

    #[test]
    fn retrieve_request_defaults_to_context_only_compact_output() {
        let request: RetrieveRequest =
            serde_json::from_value(serde_json::json!({"question": "What is cited?"})).unwrap();

        assert_eq!(request.question, "What is cited?");
        assert!(request.source_id.is_none());
        assert!(request.collection_filter.is_empty());
        assert!(request.limit.is_none());
        assert!(request.page_size.is_none());
        assert!(request.page.is_none());
        assert!(!request.fast);
        assert!(request.rerank.is_none());
        assert!(!request.bypass_cache);
        assert!(!request.include_debug);
        assert!(!request.include_locator);
        assert!(!request.passage);
    }

    #[test]
    fn health_response_defaults_missing_readiness_to_ready() {
        let response: HealthResponse =
            serde_json::from_value(serde_json::json!({"status": "ok"})).unwrap();

        assert!(response.readiness.process_alive);
        assert_eq!(response.readiness.state, "ready");
        assert!(response.readiness.retrieval_ready);
        assert_eq!(response.readiness.startup_phase, "ready");
        assert!(response.readiness.degraded_reason.is_none());
    }

    #[test]
    fn health_response_serializes_starting_readiness_fields() {
        let response = HealthResponse {
            status: "ok".into(),
            readiness: ReadinessHealth::starting(
                "orphan_recovery",
                Some("recovering previous running ingest tasks".into()),
            ),
            memory_budget: Default::default(),
            resources: Vec::new(),
            idle_reclaim: None,
            idle_exit: None,
            sqlite_durability: None,
        };

        let wire = serde_json::to_value(response).unwrap();

        assert_eq!(wire["status"], "ok");
        assert_eq!(wire["process_alive"], true);
        assert_eq!(wire["readiness"], "starting");
        assert_eq!(wire["retrieval_ready"], false);
        assert_eq!(wire["startup_phase"], "orphan_recovery");
        assert_eq!(
            wire["degraded_reason"],
            "recovering previous running ingest tasks"
        );
    }

    include!("api_retrieve_serialization_tests.rs");
}

#[cfg(test)]
#[path = "api_locator_tests.rs"]
mod api_locator_tests;
