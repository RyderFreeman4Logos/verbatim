//! HTTP API payloads shared by the daemon and thin CLI.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::collection::{
    CollectionMember, CollectionRecord, CollectionRoot, CollectionStatus, CollectionSyncReport,
};
use crate::config::ConfigReloadMetadata;
use crate::index_gc::{IndexGcApplyReport, IndexGcConfig, IndexGcPlan};
use crate::index_profile_delete::{IndexProfileDeleteApplyReport, IndexProfileDeletePlan};
use crate::task::{TaskEvent, TaskSpan, TaskSummary};
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TaskListResponse {
    #[serde(default)]
    pub tasks: Vec<TaskSummary>,
    pub total: usize,
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
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AskResponse {
    pub answer: String,
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
    pub include_debug: bool,
    #[serde(default)]
    pub include_locator: bool,
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
    pub controls: RetrieveControlsResponse,
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
    pub source_id: String,
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
    pub kind: String,
    pub derived_from: Option<String>,
    pub locator: String,
    pub structured_locator: SourceLocator,
    pub text: String,
    pub heading_path: Vec<String>,
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

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HealthResponse {
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ErrorResponse {
    pub error: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upstream_failure: Option<serde_json::Value>,
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
    use crate::types::{MarkdownBlockKind, MarkdownHeadingLocator, SourceLocator};

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
    fn retrieve_request_defaults_to_context_only_compact_output() {
        let request: RetrieveRequest =
            serde_json::from_value(serde_json::json!({"question": "What is cited?"})).unwrap();

        assert_eq!(request.question, "What is cited?");
        assert!(request.source_id.is_none());
        assert!(request.collection_filter.is_empty());
        assert!(request.limit.is_none());
        assert!(request.page_size.is_none());
        assert!(!request.fast);
        assert!(request.rerank.is_none());
        assert!(!request.include_debug);
        assert!(!request.include_locator);
    }

    #[test]
    fn retrieve_result_omits_structured_locator_until_requested() {
        let response = RetrieveResponse {
            task_id: "task-1".into(),
            query: "What is cited?".into(),
            source_id: None,
            collection_filter: None,
            embedding_profile_id: "default".into(),
            limit: 12,
            page_size: 1,
            page: 1,
            total_results: 1,
            returned_results: 1,
            controls: RetrieveControlsResponse {
                fast: false,
                rerank_enabled: false,
                dense_top_k: 80,
                bm25_top_k: 50,
                rrf_k: 60,
                rerank_top_n: 12,
            },
            timings: vec![RetrieveTimingResponse {
                phase: "retrieval".into(),
                duration_ms: 7,
            }],
            results: vec![RetrieveResultResponse {
                index: 0,
                rank: 1,
                label: "E1".into(),
                evidence_id: "ev-1".into(),
                source_id: "src-1".into(),
                source_path: Some("/tmp/doc.md".into()),
                collections: Vec::new(),
                chunk_id: "chunk-1".into(),
                kind: "text".into(),
                role: "original_text".into(),
                score: 0.03,
                locator: "/tmp/doc.md L1".into(),
                structured_locator: None,
                provenance: None,
                derived_from: None,
                snippet: "compact cited text".into(),
            }],
            debug: None,
        };

        let encoded = serde_json::to_string(&response).unwrap();

        assert!(encoded.contains("\"locator\""));
        assert!(!encoded.contains("structured_locator"));
        assert!(!encoded.contains("provenance"));
        assert!(!encoded.contains("debug"));
    }

    #[test]
    fn markdown_locator_serializes_in_evidence_and_retrieve_responses() {
        let locator = SourceLocator::Markdown {
            path: "/tmp/doc.md".into(),
            line_start: 3,
            line_end: 5,
            byte_start: 24,
            byte_end: 96,
            block_kind: MarkdownBlockKind::BlockQuote,
            block_index: 1,
            block_hash: "stable-block-hash".into(),
            heading_level: Some(2),
            heading_slug: Some("details-1".into()),
            heading_path: vec![
                MarkdownHeadingLocator {
                    level: 1,
                    text: "Intro".into(),
                    slug: "intro".into(),
                    line: 1,
                },
                MarkdownHeadingLocator {
                    level: 2,
                    text: "Details".into(),
                    slug: "details-1".into(),
                    line: 2,
                },
            ],
        };
        let evidence = EvidenceResponse {
            id: "ev-md".into(),
            source_id: "src-1".into(),
            kind: "text".into(),
            derived_from: None,
            locator: locator.to_string(),
            structured_locator: locator.clone(),
            text: "quoted markdown".into(),
            heading_path: vec!["Intro".into(), "Details".into()],
            position: 1,
            image_artifact: None,
        };
        let retrieve = RetrieveResultResponse {
            index: 0,
            rank: 1,
            label: "E1".into(),
            evidence_id: "ev-md".into(),
            source_id: "src-1".into(),
            source_path: Some("/tmp/doc.md".into()),
            collections: Vec::new(),
            chunk_id: "chunk-1".into(),
            kind: "text".into(),
            role: "original_text".into(),
            score: 0.03,
            locator: evidence.locator.clone(),
            structured_locator: Some(locator),
            provenance: None,
            derived_from: None,
            snippet: "quoted markdown".into(),
        };

        let evidence_json = serde_json::to_value(&evidence).unwrap();
        let retrieve_json = serde_json::to_value(&retrieve).unwrap();

        assert_eq!(evidence_json["structured_locator"]["type"], "Markdown");
        assert_eq!(
            evidence_json["structured_locator"]["block_kind"],
            "block_quote"
        );
        assert_eq!(
            evidence_json["structured_locator"]["heading_path"][1]["slug"],
            "details-1"
        );
        assert_eq!(retrieve_json["structured_locator"]["type"], "Markdown");
        assert_eq!(
            retrieve_json["structured_locator"]["block_hash"],
            "stable-block-hash"
        );
    }
}
