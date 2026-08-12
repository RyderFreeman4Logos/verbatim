#[cfg(test)]
use std::collections::VecDeque;
use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::convert::Infallible;
use std::fs;
use std::future::Future;
use std::io::Write;
use std::net::SocketAddr;
use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::middleware::Next;
use axum::response::sse::{Event, Sse};
use axum::response::{IntoResponse, Response};
#[cfg(test)]
use axum::routing::{get, post};
use axum::{extract::Request, Json, Router};
use futures::stream;
use futures::Stream;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::{mpsc, watch, OwnedSemaphorePermit, Semaphore};

use verbatim_core::api::{
    AddCollectionRootRequest, AddCollectionRootResponse, AddSourceRequest, AddSourceResponse,
    AppliedCollectionFilterResponse, AskCitationEvent, AskErrorEvent, AskRequest, AskResponse,
    AskTokenEvent, CheckStaleResponse, CitationResponse, CollectionFilterRequest,
    CollectionFilterResponse, CollectionResponse, CollectionResultProvenance,
    CollectionStatusResponse, CollectionSyncPathRequest, CollectionSyncRequest,
    CollectionSyncResponse, CollectionWatcherResponse, CollectionWatcherStatus,
    CollectionWatcherUpdateRequest, CollectionWatchersStatusResponse, ConfigResponse,
    CreateCollectionRequest, ErrorResponse, EvidenceResponse, GeneratedInterpretationResponse,
    HealthResponse, IdleExitActivitySnapshot, IdleExitHealth, IdleReclaimActivitySnapshot,
    IdleReclaimBackendResult, IdleReclaimCycleResult, IdleReclaimHealth, ImageArtifactResponse,
    IndexGcRequest, IndexGcResponse, IndexProfileDeleteRequest, IndexProfileDeleteResponse,
    IndexStatusResponse, IngestResponse, ReadinessHealth, ReindexRequest, ReindexResponse,
    RetrieveControlsResponse, RetrieveRequest, RetrieveResponse, RetrieveResultResponse,
    RetrieveTimingResponse, SourceResponse, TaskCreatedResponse, TaskEmbeddingWaitAggregate,
    TaskEventsResponse, TaskIngestRequest, TaskListAggregate, TaskListResponse,
    TaskProfileResponse, TaskQueueTurnover, TaskQueueTurnoverWindow, TaskReasonBucket,
    TaskStaleRunningAggregate, TaskSummaryResponse, TaskWaitEvent, VectorJsonCleanupRequest,
    VectorJsonCleanupResponse,
};
use verbatim_core::collection::{
    diff_collection_members, validate_collection_name, CollectionIgnoreRules, CollectionMember,
    CollectionMemberCandidate, CollectionRecord, CollectionSyncPathInput,
};
use verbatim_core::config::{
    self, Config, ConfigReloadMetadata, ConfigRestartRequiredKey, DaemonIdleExitConfig,
    DaemonIdleReclaimConfig, DaemonResourceConfig, RerankConfig, RerankStrategy, RetrievalConfig,
    SQLITE_WRITER_ACTIVE_CAPACITY,
};
use verbatim_core::embed::OpenAiEmbeddingClient;
use verbatim_core::generate::{
    image_artifact_evidence_id, select_image_attachments, GenerationCallTelemetry,
    GenerationContext, GenerationTelemetry, GenerationVerificationStatus, Generator,
};
use verbatim_core::graphrag::GraphRagService;
use verbatim_core::index::sqlite_fts::FtsMaintenanceOutcome;
use verbatim_core::index_gc::{apply_index_gc, plan_index_gc, IndexGcApplyReport};
use verbatim_core::ingest::{
    IndexingOutcome, IngestPipeline, SourceIngestFreshness, SourceIngestOutcome,
};
use verbatim_core::memory_budget::MemoryBudget;
use verbatim_core::ocr::source_ingest_diagnostics;
use verbatim_core::provider::openai_compatible::{
    model_endpoint_resource_snapshots, OpenAiCompatibleLlmReranker, OpenAiCompatibleReranker,
};
use verbatim_core::provider::ProviderError;
use verbatim_core::resource::{
    global_resource_registry, ObservableResource, ResourceLimitConfig, ResourceQueueSnapshot,
};
use verbatim_core::retrieval_telemetry::SpanKind;
use verbatim_core::retrieve::{
    refresh_evidence_pack_debug, RetrievalCanonicalSelectionBudget, RetrievalDebugOptions,
    RetrievalDisplayScope, RetrievalPipeline,
};
use verbatim_core::store::{Store, TaskListFilter};
use verbatim_core::task::{
    ask_request_metadata, ask_result_metadata, bounded_error, bounded_json, ingest_result_metadata,
    ingest_result_metadata_with_skips, ingest_task_request_metadata_with_queue_claim,
    ingest_task_request_metadata_with_queue_claim_and_batch, reindex_result_metadata,
    reindex_task_request_metadata_with_queue_claim,
    reindex_task_request_metadata_with_queue_claim_and_batch, retrieve_request_metadata,
    retrieve_result_metadata, AskGenerationStageProfile, AskGenerationStatus,
    AskOutputStageProfile, AskTaskProfile, AskVerificationStageProfile, AskVerificationStatus,
    IngestTaskStage, PhaseTiming, RetrieveDenseStageProfile, RetrieveDisplayStageProfile,
    RetrieveEvidenceStageProfile, RetrieveRerankStageProfile, RetrieveStageProfile,
    RetrieveTaskProfile, TaskCollectionFilterControls, TaskEndpointSummary, TaskEvent,
    TaskFilterControls, TaskId, TaskKind, TaskOutputControls, TaskProfile, TaskProfileControls,
    TaskProgressSnapshot, TaskQdrantControls, TaskRerankControls, TaskResourceProfile,
    TaskRetrievalControls, TaskSourceFilterControls, TaskSpan, TaskStatus, TaskSummary,
    TaskVectorControls,
};
use verbatim_core::traits::{EmbeddingClient, EmbeddingEndpointCapabilities};
use verbatim_core::types::{
    CanonicalLocator, CitationRef, EmbeddingCacheStats, EmbeddingProfileId, EvidenceId,
    EvidenceKind, EvidenceUnit, ImageArtifact, ReferenceComponent, RetrievalDebug,
    RetrievalDenseVectorPath, RetrievalEvidencePackEntry, RetrievalEvidenceRole,
    RetrievalLocalSpansMs, RetrievalRerankStatus, RetrievalResult, SourceId, SourceLocator,
    SourceStatus,
};
use verbatim_core::upstream::{sanitize_text, UpstreamFailureError};

#[path = "auth_middleware.rs"]
mod auth_middleware;
#[path = "deletion_api.rs"]
mod deletion_api;
#[path = "routes.rs"]
mod routes;
#[path = "source_relocation_api.rs"]
mod source_relocation_api;
#[path = "sqlite_durability_ops.rs"]
mod sqlite_durability_ops;

use deletion_api::{
    delete_source, list_deletion_reports, reconcile_deletions_on_startup,
    start_deletion_reconcile_scheduler, STARTUP_DELETION_RECONCILE_BATCH_SIZE,
};
use source_relocation_api::relocate_source;

// ---------------------------------------------------------------------------
// Shared state
//
// `IngestPipeline` contains a rusqlite `Connection` which is `!Send`.
// axum requires handler futures to be `Send`, so we cannot hold a
// `tokio::sync::MutexGuard<IngestPipeline>` across `.await` points.
//
// Strategy: wrap the pipeline in a std Mutex (sync-only access within
// `spawn_blocking`) and keep the Send-safe async clients outside.
// ---------------------------------------------------------------------------

struct AppState {
    /// Pipeline slot. Long indexing operations take ownership, release this mutex,
    /// then restore the pipeline after completion.
    pipeline: std::sync::Mutex<Option<IngestPipeline>>,
    /// Independent task metadata connection for serialized writes.
    task_store: std::sync::Mutex<Store>,
    index_status_cache: std::sync::RwLock<Option<IndexStatusResponse>>,
    readiness: std::sync::RwLock<ReadinessHealth>,
    resources: DaemonResources,
    memory_budget: MemoryBudget,
    ingest_queue_active: AtomicBool,
    #[cfg(test)]
    ingest_queue_drain_receipt: std::sync::Mutex<Option<tokio::sync::oneshot::Sender<()>>>,
    /// Actual indexing worker occupancy, independent from persisted task status.
    ingest_worker_active: AtomicBool,
    collection_watcher: CollectionWatcherRuntime,
    idle_reclaim: Arc<IdleReclaimRuntime>,
    idle_exit: Arc<IdleExitRuntime>,
    #[cfg(test)]
    idle_reclaim_before_backend_hook: std::sync::Mutex<Option<IdleReclaimBeforeBackendHook>>,
    #[cfg(test)]
    idle_reclaim_before_backend_call_hook: std::sync::Mutex<Option<IdleReclaimBeforeBackendHook>>,
    runtime_config: std::sync::RwLock<RuntimeConfigState>,
    config_path: PathBuf,
    data_dir: PathBuf,
}

type SharedState = Arc<AppState>;

#[derive(Clone)]
struct RuntimeConfigState {
    config: Config,
    reload: ConfigReloadMetadata,
}

#[derive(Clone)]
struct DaemonResources {
    sqlite_writer: Arc<ObservableResource>,
    sqlite_reader: Arc<ObservableResource>,
    vector_search: Arc<ObservableResource>,
    cpu_worker: Arc<ObservableResource>,
    index_publish: Arc<ObservableResource>,
    qdrant_upsert: Arc<ObservableResource>,
}

const ASK_STREAM_EVENT_BUFFER: usize = 32;
const TASK_WAIT_EVENT_BUFFER: usize = 16;
const TASK_WAIT_EVENT_LIMIT: usize = 100;
const TASK_TELEMETRY_REDACTED: &str = "<redacted>";
const TASK_LIST_DEFAULT_LIMIT: usize = 20;
const TASK_LIST_MAX_LIMIT: usize = 100;
const TASK_QUEUE_TURNOVER_EVENT_LIMIT: usize = 1000;
const TASK_QUEUE_AGGREGATE_ACTIVE_SAMPLE_LIMIT: usize = 100;
const TASK_QUEUE_REASON_BUCKET_LIMIT: usize = 16;
const TASK_WAIT_POLL_INTERVAL: Duration = Duration::from_secs(1);
const FAST_RETRIEVAL_TOP_K: usize = 20;
const DEFAULT_SNIPPET_CHARS: usize = 240;
const TASK_PROFILE_COLLECTION_SAMPLE_LIMIT: usize = 8;
const TASK_PROFILE_RESOURCE_QUEUE_LIMIT: usize = 32;
const TASK_PROFILE_STRING_MAX_CHARS: usize = 256;
const CONFIG_RELOAD_DEBOUNCE: Duration = Duration::from_millis(250);
const CONFIG_RELOAD_ERROR_MAX_CHARS: usize = 1024;
const COLLECTION_WATCHER_EVENT_BUFFER: usize = 512;
const COLLECTION_WATCHER_STATUS_ERROR_MAX_CHARS: usize = 1024;
const IDLE_RECLAIM_DISABLED_POLL: Duration = Duration::from_secs(60);
const IDLE_RECLAIM_MAX_POLL: Duration = Duration::from_secs(60);
const IDLE_RECLAIM_MIN_POLL: Duration = Duration::from_secs(1);
const IDLE_RECLAIM_ADMISSION_PERMITS: u32 = 1_000_000;
const IDLE_EXIT_DISABLED_POLL: Duration = Duration::from_secs(60);
const IDLE_EXIT_MAX_POLL: Duration = Duration::from_secs(60);
const IDLE_EXIT_MIN_POLL: Duration = Duration::from_secs(1);
const DAEMON_RUNTIME_SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Default)]
struct CollectionWatcherRuntime {
    tx: std::sync::Mutex<Option<mpsc::Sender<CollectionWatcherCommand>>>,
    statuses: std::sync::Mutex<HashMap<String, CollectionWatcherStatusState>>,
}

struct IdleReclaimRuntime {
    admission: Arc<Semaphore>,
    active_http_requests: AtomicU64,
    active_sse_streams: AtomicU64,
    last_activity_unix_ms: AtomicU64,
    running: AtomicBool,
    last_result: std::sync::Mutex<Option<IdleReclaimCycleResult>>,
    last_attempt_result: std::sync::Mutex<Option<IdleReclaimCycleResult>>,
}

impl IdleReclaimRuntime {
    fn new(now_unix_ms: u64) -> Self {
        Self {
            admission: Arc::new(Semaphore::new(IDLE_RECLAIM_ADMISSION_PERMITS as usize)),
            active_http_requests: AtomicU64::new(0),
            active_sse_streams: AtomicU64::new(0),
            last_activity_unix_ms: AtomicU64::new(now_unix_ms),
            running: AtomicBool::new(false),
            last_result: std::sync::Mutex::new(None),
            last_attempt_result: std::sync::Mutex::new(None),
        }
    }

    async fn start_http(self: &Arc<Self>) -> ActivityGuard {
        let permit = Arc::clone(&self.admission)
            .acquire_owned()
            .await
            .expect("idle reclaim admission semaphore is never closed");
        self.start_activity(ActivityKind::Http, permit)
    }

    async fn start_sse(self: &Arc<Self>) -> ActivityGuard {
        let permit = Arc::clone(&self.admission)
            .acquire_owned()
            .await
            .expect("idle reclaim admission semaphore is never closed");
        self.start_activity(ActivityKind::Sse, permit)
    }

    async fn start_backend(self: &Arc<Self>) -> IdleReclaimBackendAdmission {
        let permit = Arc::clone(&self.admission)
            .acquire_many_owned(IDLE_RECLAIM_ADMISSION_PERMITS)
            .await
            .expect("idle reclaim admission semaphore is never closed");
        IdleReclaimBackendAdmission { _permit: permit }
    }

    async fn start_untracked_admission(self: &Arc<Self>) -> ActivityGuard {
        let permit = Arc::clone(&self.admission)
            .acquire_owned()
            .await
            .expect("idle reclaim admission semaphore is never closed");
        ActivityGuard {
            runtime: Arc::clone(self),
            kind: None,
            _admission_permit: permit,
        }
    }

    #[cfg(test)]
    fn try_start_http_for_test(self: &Arc<Self>) -> Option<ActivityGuard> {
        let permit = Arc::clone(&self.admission).try_acquire_owned().ok()?;
        Some(self.start_activity(ActivityKind::Http, permit))
    }

    fn start_activity(
        self: &Arc<Self>,
        kind: ActivityKind,
        _admission_permit: OwnedSemaphorePermit,
    ) -> ActivityGuard {
        match kind {
            ActivityKind::Http => {
                self.active_http_requests.fetch_add(1, Ordering::AcqRel);
            }
            ActivityKind::Sse => {
                self.active_sse_streams.fetch_add(1, Ordering::AcqRel);
            }
        }
        self.mark_activity();
        ActivityGuard {
            runtime: Arc::clone(self),
            kind: Some(kind),
            _admission_permit,
        }
    }

    fn mark_activity(&self) {
        self.last_activity_unix_ms
            .store(now_unix_millis(), Ordering::Release);
    }

    fn active_http_requests(&self) -> u64 {
        self.active_http_requests.load(Ordering::Acquire)
    }

    fn active_sse_streams(&self) -> u64 {
        self.active_sse_streams.load(Ordering::Acquire)
    }

    fn last_activity_unix_ms(&self) -> u64 {
        self.last_activity_unix_ms.load(Ordering::Acquire)
    }

    fn last_result(&self) -> Option<IdleReclaimCycleResult> {
        self.last_result
            .lock()
            .map(|result| result.clone())
            .unwrap_or(None)
    }

    fn last_attempt_result(&self) -> Option<IdleReclaimCycleResult> {
        self.last_attempt_result
            .lock()
            .map(|result| result.clone())
            .unwrap_or(None)
    }

    fn record_result(&self, result: IdleReclaimCycleResult) {
        if idle_reclaim_result_attempted(&result) {
            if let Ok(mut last_attempt_result) = self.last_attempt_result.lock() {
                *last_attempt_result = Some(result.clone());
            }
        }
        if let Ok(mut last_result) = self.last_result.lock() {
            *last_result = Some(result);
        }
    }
}

struct IdleExitRuntime {
    active_http_requests: AtomicU64,
    active_sse_streams: AtomicU64,
    last_activity_unix_ms: AtomicU64,
    had_blockers: AtomicBool,
    shutdown_requested: AtomicBool,
    watcher_resync_requested: AtomicBool,
}

impl IdleExitRuntime {
    fn new(now_unix_ms: u64) -> Self {
        Self {
            active_http_requests: AtomicU64::new(0),
            active_sse_streams: AtomicU64::new(0),
            last_activity_unix_ms: AtomicU64::new(now_unix_ms),
            had_blockers: AtomicBool::new(false),
            shutdown_requested: AtomicBool::new(false),
            watcher_resync_requested: AtomicBool::new(false),
        }
    }

    fn start_http(self: &Arc<Self>) -> IdleExitActivityGuard {
        self.start_activity(IdleExitActivityKind::Http)
    }

    fn start_sse(self: &Arc<Self>) -> IdleExitActivityGuard {
        self.start_activity(IdleExitActivityKind::Sse)
    }

    fn start_activity(self: &Arc<Self>, kind: IdleExitActivityKind) -> IdleExitActivityGuard {
        match kind {
            IdleExitActivityKind::Http => {
                self.active_http_requests.fetch_add(1, Ordering::AcqRel);
            }
            IdleExitActivityKind::Sse => {
                self.active_sse_streams.fetch_add(1, Ordering::AcqRel);
            }
        }
        self.mark_activity();
        IdleExitActivityGuard {
            runtime: Arc::clone(self),
            kind,
        }
    }

    fn mark_activity(&self) {
        self.last_activity_unix_ms
            .store(now_unix_millis(), Ordering::Release);
    }

    fn observe_blockers(&self, has_blockers: bool) {
        let previously_blocked = self.had_blockers.swap(has_blockers, Ordering::AcqRel);
        if previously_blocked && !has_blockers {
            self.mark_activity();
        }
    }

    fn active_http_requests(&self) -> u64 {
        self.active_http_requests.load(Ordering::Acquire)
    }

    fn active_sse_streams(&self) -> u64 {
        self.active_sse_streams.load(Ordering::Acquire)
    }

    fn last_activity_unix_ms(&self) -> u64 {
        self.last_activity_unix_ms.load(Ordering::Acquire)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum IdleExitActivityKind {
    Http,
    Sse,
}

struct IdleExitActivityGuard {
    runtime: Arc<IdleExitRuntime>,
    kind: IdleExitActivityKind,
}

impl Drop for IdleExitActivityGuard {
    fn drop(&mut self) {
        match self.kind {
            IdleExitActivityKind::Http => {
                self.runtime
                    .active_http_requests
                    .fetch_sub(1, Ordering::AcqRel);
            }
            IdleExitActivityKind::Sse => {
                self.runtime
                    .active_sse_streams
                    .fetch_sub(1, Ordering::AcqRel);
            }
        }
        self.runtime.mark_activity();
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActivityKind {
    Http,
    Sse,
}

struct ActivityGuard {
    runtime: Arc<IdleReclaimRuntime>,
    kind: Option<ActivityKind>,
    _admission_permit: OwnedSemaphorePermit,
}

impl Drop for ActivityGuard {
    fn drop(&mut self) {
        let Some(kind) = self.kind else {
            return;
        };
        match kind {
            ActivityKind::Http => {
                self.runtime
                    .active_http_requests
                    .fetch_sub(1, Ordering::AcqRel);
            }
            ActivityKind::Sse => {
                self.runtime
                    .active_sse_streams
                    .fetch_sub(1, Ordering::AcqRel);
            }
        }
        self.runtime.mark_activity();
    }
}

struct IdleReclaimBackendAdmission {
    _permit: OwnedSemaphorePermit,
}

#[derive(Debug, Clone)]
struct IdleReclaimGate {
    health: IdleReclaimHealth,
}

#[derive(Debug, Clone)]
struct IdleExitGate {
    health: IdleExitHealth,
}

#[cfg(test)]
type IdleReclaimBeforeBackendHook = Box<dyn FnMut(&SharedState) + Send + 'static>;

#[derive(Debug, Clone, Default)]
struct CollectionWatcherStatusState {
    active: bool,
    ignored_by_config: bool,
    watched_root_count: usize,
    pending_event_count: usize,
    last_event_at: Option<String>,
    last_sync_at: Option<String>,
    last_error: Option<String>,
    last_added: usize,
    last_removed: usize,
    last_unchanged: usize,
    last_task_id: Option<String>,
}

#[derive(Debug)]
enum CollectionWatcherCommand {
    FilesystemEvent { paths: Vec<PathBuf> },
    NotifyError { error: String },
    Refresh,
    ResyncActive,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct DebouncedCollectionSet {
    pending: BTreeSet<String>,
}

impl DebouncedCollectionSet {
    fn insert_many<I>(&mut self, names: I) -> bool
    where
        I: IntoIterator<Item = String>,
    {
        let before = self.pending.len();
        self.pending.extend(names);
        self.pending.len() != before
    }

    fn drain(&mut self) -> Vec<String> {
        std::mem::take(&mut self.pending).into_iter().collect()
    }

    fn is_empty(&self) -> bool {
        self.pending.is_empty()
    }
}

fn elapsed_ms(started: Instant) -> u64 {
    started.elapsed().as_millis().try_into().unwrap_or(u64::MAX)
}

fn unix_seconds_delta_ms(start: &str, end: &str) -> Option<u64> {
    let start = start.parse::<u64>().ok()?;
    let end = end.parse::<u64>().ok()?;
    Some(end.saturating_sub(start).saturating_mul(1_000))
}

fn now_unix_millis() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis().try_into().unwrap_or(u64::MAX))
        .unwrap_or(0)
}

async fn track_http_activity(
    State(state): State<SharedState>,
    request: Request,
    next: Next,
) -> Response {
    let request_path = request.uri().path().to_string();
    let is_health_request = request_path == "/api/health";
    if let Some(status) = idle_exit_shutdown_rejection_status(&state, &request_path) {
        return (
            status,
            Json(ErrorResponse::new(
                "daemon is shutting down after idle timeout; retry after the service restarts",
            )),
        )
            .into_response();
    }
    let should_track_idle_exit =
        idle_exit_tracks_request_path(&idle_exit_config(&state), &request_path);
    let _idle_exit_guard = if should_track_idle_exit {
        Some(state.idle_exit.start_http())
    } else {
        None
    };
    if let Some(status) = idle_exit_shutdown_rejection_status(&state, &request_path) {
        return (
            status,
            Json(ErrorResponse::new(
                "daemon is shutting down after idle timeout; retry after the service restarts",
            )),
        )
            .into_response();
    }
    if should_track_idle_exit && !is_health_request {
        queue_idle_exit_collection_watcher_resync_if_enabled(&state);
    }
    let _guard = if is_health_request {
        None
    } else {
        Some(state.idle_reclaim.start_http().await)
    };
    let _health_admission = if is_health_request {
        Some(state.idle_reclaim.start_untracked_admission().await)
    } else {
        None
    };
    next.run(request).await
}

fn idle_exit_tracks_request_path(config: &DaemonIdleExitConfig, path: &str) -> bool {
    path != "/api/health" || config.count_health_requests
}

fn idle_exit_shutdown_rejection_status(state: &SharedState, path: &str) -> Option<StatusCode> {
    if path == "/api/health" {
        return None;
    }
    state
        .idle_exit
        .shutdown_requested
        .load(Ordering::Acquire)
        .then_some(StatusCode::SERVICE_UNAVAILABLE)
}

fn idle_exit_config(state: &SharedState) -> DaemonIdleExitConfig {
    runtime_config_snapshot(state)
        .map(|runtime| runtime.config.daemon.idle_exit.bounded())
        .unwrap_or_default()
}

fn idle_reclaim_config(state: &SharedState) -> DaemonIdleReclaimConfig {
    runtime_config_snapshot(state)
        .map(|runtime| runtime.config.daemon.idle_reclaim.bounded())
        .unwrap_or_default()
}

fn idle_reclaim_gate(
    state: &SharedState,
    resources: Vec<ResourceQueueSnapshot>,
) -> IdleReclaimGate {
    idle_reclaim_gate_with_running(
        state,
        resources,
        state.idle_reclaim.running.load(Ordering::Acquire),
    )
}

fn idle_reclaim_gate_with_running(
    state: &SharedState,
    resources: Vec<ResourceQueueSnapshot>,
    running: bool,
) -> IdleReclaimGate {
    let config = idle_reclaim_config(state);
    let now = now_unix_millis();
    let last_activity = state.idle_reclaim.last_activity_unix_ms();
    let idle_for_millis = now.saturating_sub(last_activity);
    let idle_timeout_millis = millis_from_secs(config.idle_timeout_seconds);
    let min_interval_millis = millis_from_secs(config.min_interval_seconds);
    let active_tasks = active_task_count(state).unwrap_or(usize::MAX);
    let activity = IdleReclaimActivitySnapshot {
        http_requests: state.idle_reclaim.active_http_requests(),
        sse_streams: state.idle_reclaim.active_sse_streams(),
        active_tasks,
        resource_active: resources.iter().map(|resource| resource.active).sum(),
        resource_queued: resources.iter().map(|resource| resource.queued).sum(),
        ingest_queue_active: state.ingest_queue_active.load(Ordering::Acquire),
        ingest_worker_active: state.ingest_worker_active.load(Ordering::Acquire),
        pipeline_busy: pipeline_busy_for_idle_reclaim(state),
    };
    let last_attempt_at = state
        .idle_reclaim
        .last_attempt_result()
        .map(|result| result.attempted_at_unix_ms);
    let min_interval_remaining = last_attempt_at
        .map(|attempted_at| {
            attempted_at
                .saturating_add(min_interval_millis)
                .saturating_sub(now)
        })
        .unwrap_or(0);
    let idle_remaining = idle_timeout_millis.saturating_sub(idle_for_millis);
    let skip_reason = idle_reclaim_skip_reason(
        &config,
        &activity,
        idle_remaining,
        min_interval_remaining,
        running,
    );
    let currently_idle = idle_reclaim_activity_is_idle(&activity);
    let eligible = config.enabled && currently_idle && skip_reason.is_none();
    let next_eligible_in_millis = if eligible {
        None
    } else if config.enabled {
        Some(idle_remaining.max(min_interval_remaining))
    } else {
        None
    };

    IdleReclaimGate {
        health: IdleReclaimHealth {
            enabled: config.enabled,
            sqlite_shrink_memory: config.sqlite_shrink_memory,
            malloc_trim: config.malloc_trim,
            currently_idle,
            eligible,
            skip_reason,
            idle_for_millis,
            idle_timeout_millis,
            min_interval_millis,
            next_eligible_in_millis,
            active: activity,
            last_result: state.idle_reclaim.last_result(),
            last_attempt_result: state.idle_reclaim.last_attempt_result(),
        },
    }
}

fn idle_exit_gate(state: &SharedState, resources: Vec<ResourceQueueSnapshot>) -> IdleExitGate {
    idle_exit_gate_inner(state, resources, true, None)
}

fn idle_exit_health_gate(
    state: &SharedState,
    resources: Vec<ResourceQueueSnapshot>,
) -> IdleExitGate {
    idle_exit_gate_inner(state, resources, false, None)
}

fn idle_exit_gate_ignoring_shutdown(
    state: &SharedState,
    resources: Vec<ResourceQueueSnapshot>,
) -> IdleExitGate {
    idle_exit_gate_inner(state, resources, true, Some(false))
}

fn idle_exit_gate_inner(
    state: &SharedState,
    resources: Vec<ResourceQueueSnapshot>,
    observe_blocker_transition: bool,
    shutdown_requested_override: Option<bool>,
) -> IdleExitGate {
    let config = idle_exit_config(state);
    let active_tasks = active_task_count(state).unwrap_or(usize::MAX);
    let watcher = collection_watcher_idle_exit_snapshot(state);
    let activity = IdleExitActivitySnapshot {
        http_requests: state.idle_exit.active_http_requests(),
        sse_streams: state.idle_exit.active_sse_streams(),
        active_tasks,
        resource_active: resources.iter().map(|resource| resource.active).sum(),
        resource_queued: resources.iter().map(|resource| resource.queued).sum(),
        ingest_queue_active: state.ingest_queue_active.load(Ordering::Acquire),
        ingest_worker_active: state.ingest_worker_active.load(Ordering::Acquire),
        pipeline_busy: pipeline_busy_for_idle_reclaim(state),
        watched_roots: watcher.watched_roots,
        pending_watcher_events: watcher.pending_events,
    };
    let has_blockers = idle_exit_activity_has_blockers(&config, &activity);
    if observe_blocker_transition {
        state.idle_exit.observe_blockers(has_blockers);
    }

    let now = now_unix_millis();
    let last_activity = state.idle_exit.last_activity_unix_ms();
    let idle_for_millis = now.saturating_sub(last_activity);
    let timeout_millis = millis_from_secs(config.timeout_seconds);
    let idle_remaining = timeout_millis.saturating_sub(idle_for_millis);
    let deadline_unix_ms = last_activity.saturating_add(timeout_millis);
    let shutdown_requested = shutdown_requested_override
        .unwrap_or_else(|| state.idle_exit.shutdown_requested.load(Ordering::Acquire));
    let skip_reason = idle_exit_skip_reason(&config, &activity, idle_remaining, shutdown_requested);
    let currently_idle = !has_blockers;
    let eligible = config.enabled && currently_idle && skip_reason.is_none();
    let next_eligible_in_millis = if eligible || !config.enabled {
        None
    } else if has_blockers {
        Some(timeout_millis)
    } else {
        Some(idle_remaining)
    };

    IdleExitGate {
        health: IdleExitHealth {
            enabled: config.enabled,
            count_health_requests: config.count_health_requests,
            allow_with_collection_watcher: config.allow_with_collection_watcher,
            auto_start_on_cli: config.auto_start_on_cli,
            currently_idle,
            eligible,
            skip_reason,
            idle_for_millis,
            timeout_millis,
            last_activity_unix_ms: last_activity,
            deadline_unix_ms,
            next_eligible_in_millis,
            active: activity,
        },
    }
}

#[derive(Debug, Clone, Copy, Default)]
struct CollectionWatcherIdleExitSnapshot {
    watched_roots: usize,
    pending_events: usize,
}

fn collection_watcher_idle_exit_snapshot(state: &SharedState) -> CollectionWatcherIdleExitSnapshot {
    let pending_resync = if state
        .idle_exit
        .watcher_resync_requested
        .load(Ordering::Acquire)
    {
        1
    } else {
        0
    };
    match state.collection_watcher.statuses.lock() {
        Ok(statuses) => CollectionWatcherIdleExitSnapshot {
            watched_roots: statuses
                .values()
                .filter(|status| status.active)
                .map(|status| status.watched_root_count)
                .sum(),
            pending_events: statuses
                .values()
                .map(|status| status.pending_event_count)
                .sum::<usize>()
                .saturating_add(pending_resync),
        },
        Err(error) => {
            tracing::warn!(error = %error, "failed to inspect collection watcher idle-exit blockers");
            CollectionWatcherIdleExitSnapshot {
                watched_roots: usize::MAX,
                pending_events: usize::MAX,
            }
        }
    }
}

fn idle_exit_activity_has_blockers(
    config: &DaemonIdleExitConfig,
    activity: &IdleExitActivitySnapshot,
) -> bool {
    activity.http_requests > 0
        || activity.sse_streams > 0
        || activity.active_tasks > 0
        || activity.resource_active > 0
        || activity.resource_queued > 0
        || activity.ingest_queue_active
        || activity.ingest_worker_active
        || activity.pipeline_busy
        || activity.pending_watcher_events > 0
        || (!config.allow_with_collection_watcher && activity.watched_roots > 0)
}

fn idle_exit_skip_reason(
    config: &DaemonIdleExitConfig,
    activity: &IdleExitActivitySnapshot,
    idle_remaining: u64,
    shutdown_requested: bool,
) -> Option<String> {
    if !config.enabled {
        return Some("disabled".into());
    }
    if shutdown_requested {
        return Some("shutdown_requested".into());
    }
    if activity.http_requests > 0 {
        return Some("active_http_requests".into());
    }
    if activity.sse_streams > 0 {
        return Some("active_sse_streams".into());
    }
    if activity.active_tasks > 0 {
        return Some("active_tasks".into());
    }
    if activity.resource_active > 0 {
        return Some("active_resources".into());
    }
    if activity.resource_queued > 0 {
        return Some("queued_resources".into());
    }
    if activity.ingest_queue_active {
        return Some("ingest_queue_active".into());
    }
    if activity.ingest_worker_active {
        return Some("ingest_worker_active".into());
    }
    if activity.pipeline_busy {
        return Some("pipeline_busy".into());
    }
    if activity.pending_watcher_events > 0 {
        return Some("pending_collection_watcher_events".into());
    }
    if !config.allow_with_collection_watcher && activity.watched_roots > 0 {
        return Some("active_collection_watchers".into());
    }
    if idle_remaining > 0 {
        return Some("idle_timeout_not_reached".into());
    }
    None
}

fn millis_from_secs(seconds: u64) -> u64 {
    seconds.saturating_mul(1_000)
}

fn active_task_count(state: &SharedState) -> Result<usize> {
    let store = state
        .task_store
        .lock()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok(store
        .list_tasks_page(TaskListFilter::Active, 1)
        .context("count active tasks for idle reclaim gate")?
        .total)
}

fn pipeline_busy_for_idle_reclaim(state: &SharedState) -> bool {
    match state.pipeline.try_lock() {
        Ok(pipeline) => pipeline.is_none(),
        Err(std::sync::TryLockError::WouldBlock) => true,
        Err(std::sync::TryLockError::Poisoned(_)) => true,
    }
}

fn idle_reclaim_activity_is_idle(activity: &IdleReclaimActivitySnapshot) -> bool {
    activity.http_requests == 0
        && activity.sse_streams == 0
        && activity.active_tasks == 0
        && activity.resource_active == 0
        && activity.resource_queued == 0
        && !activity.ingest_queue_active
        && !activity.ingest_worker_active
        && !activity.pipeline_busy
}

fn idle_reclaim_skip_reason(
    config: &DaemonIdleReclaimConfig,
    activity: &IdleReclaimActivitySnapshot,
    idle_remaining: u64,
    min_interval_remaining: u64,
    running: bool,
) -> Option<String> {
    if !config.enabled {
        return Some("disabled".into());
    }
    if running {
        return Some("already_running".into());
    }
    if activity.http_requests > 0 {
        return Some("active_http_requests".into());
    }
    if activity.sse_streams > 0 {
        return Some("active_sse_streams".into());
    }
    if activity.active_tasks > 0 {
        return Some("active_tasks".into());
    }
    if activity.resource_active > 0 {
        return Some("active_resources".into());
    }
    if activity.resource_queued > 0 {
        return Some("queued_resources".into());
    }
    if activity.ingest_queue_active {
        return Some("ingest_queue_active".into());
    }
    if activity.ingest_worker_active {
        return Some("ingest_worker_active".into());
    }
    if activity.pipeline_busy {
        return Some("pipeline_busy".into());
    }
    if idle_remaining > 0 {
        return Some("idle_timeout_not_reached".into());
    }
    if min_interval_remaining > 0 {
        return Some("min_interval_not_reached".into());
    }
    None
}

fn start_idle_reclaim_scheduler(state: SharedState) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(idle_reclaim_poll_delay(&state)).await;
            let result = run_idle_reclaim_cycle_if_due(&state).await;
            if result.status == "failed" || result.status == "partial_failure" {
                tracing::warn!(
                    status = %result.status,
                    skip_reason = ?result.skip_reason,
                    sqlite_status = %result.sqlite.status,
                    allocator_status = %result.allocator.status,
                    "idle memory reclaim completed with non-fatal errors"
                );
            }
        }
    })
}

fn idle_reclaim_poll_delay(state: &SharedState) -> Duration {
    let config = idle_reclaim_config(state);
    if !config.enabled {
        return IDLE_RECLAIM_DISABLED_POLL;
    }
    Duration::from_secs(config.idle_timeout_seconds.min(config.min_interval_seconds))
        .max(IDLE_RECLAIM_MIN_POLL)
        .min(IDLE_RECLAIM_MAX_POLL)
}

fn start_idle_exit_scheduler(
    state: SharedState,
    shutdown_tx: watch::Sender<bool>,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        loop {
            tokio::time::sleep(idle_exit_poll_delay(&state)).await;
            if run_idle_exit_cycle_if_due(&state) {
                let _ = shutdown_tx.send(true);
                break;
            }
        }
    })
}

fn idle_exit_poll_delay(state: &SharedState) -> Duration {
    let config = idle_exit_config(state);
    if !config.enabled {
        return IDLE_EXIT_DISABLED_POLL;
    }
    Duration::from_secs(config.timeout_seconds)
        .max(IDLE_EXIT_MIN_POLL)
        .min(IDLE_EXIT_MAX_POLL)
}

fn run_idle_exit_cycle_if_due(state: &SharedState) -> bool {
    run_idle_exit_cycle_if_due_with_resources(state, daemon_resource_snapshots(state), || {
        daemon_resource_snapshots(state)
    })
}

fn run_idle_exit_cycle_if_due_with_resources<F>(
    state: &SharedState,
    resources: Vec<ResourceQueueSnapshot>,
    confirmation_resources: F,
) -> bool
where
    F: FnOnce() -> Vec<ResourceQueueSnapshot>,
{
    let gate = idle_exit_gate(state, resources);
    if !gate.health.eligible {
        return false;
    }
    if state
        .idle_exit
        .shutdown_requested
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return false;
    }
    let confirmation = match confirm_idle_exit_shutdown_after_admission_closed_with_resources(
        state,
        confirmation_resources(),
    ) {
        Some(gate) => gate,
        None => return false,
    };
    tracing::info!(
        reason = "idle_timeout",
        timeout_seconds = idle_exit_config(state).timeout_seconds,
        idle_for_millis = confirmation.health.idle_for_millis,
        deadline_unix_ms = confirmation.health.deadline_unix_ms,
        "daemon idle exit timeout elapsed; requesting graceful shutdown"
    );
    true
}

#[cfg(test)]
fn confirm_idle_exit_shutdown_after_admission_closed(state: &SharedState) -> Option<IdleExitGate> {
    confirm_idle_exit_shutdown_after_admission_closed_with_resources(
        state,
        daemon_resource_snapshots(state),
    )
}

fn confirm_idle_exit_shutdown_after_admission_closed_with_resources(
    state: &SharedState,
    resources: Vec<ResourceQueueSnapshot>,
) -> Option<IdleExitGate> {
    let confirmation = idle_exit_gate_ignoring_shutdown(state, resources);
    if confirmation.health.eligible {
        return Some(confirmation);
    }
    state
        .idle_exit
        .shutdown_requested
        .store(false, Ordering::Release);
    tracing::debug!(
        skip_reason = ?confirmation.health.skip_reason,
        "idle exit shutdown aborted because activity arrived while admission was closing"
    );
    None
}

async fn run_idle_reclaim_cycle_if_due(state: &SharedState) -> IdleReclaimCycleResult {
    let gate = idle_reclaim_gate_with_running(state, daemon_resource_snapshots(state), false);
    if let Some(skip_reason) = gate.health.skip_reason.clone() {
        let result = skipped_idle_reclaim_result(skip_reason);
        state.idle_reclaim.record_result(result.clone());
        return result;
    }
    if state
        .idle_reclaim
        .running
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        let result = skipped_idle_reclaim_result("already_running".into());
        state.idle_reclaim.record_result(result.clone());
        return result;
    }
    let _lease = IdleReclaimRunLease {
        runtime: Arc::clone(&state.idle_reclaim),
    };

    let gate = idle_reclaim_gate_with_running(state, daemon_resource_snapshots(state), false);
    if let Some(skip_reason) = gate.health.skip_reason.clone() {
        let result = skipped_idle_reclaim_result(skip_reason);
        state.idle_reclaim.record_result(result.clone());
        return result;
    }

    let config = idle_reclaim_config(state);
    let attempted_at_unix_ms = now_unix_millis();
    maybe_run_idle_reclaim_before_backend_hook(state);
    if let Some(skip_reason) = idle_reclaim_backend_skip_reason(state) {
        let result = skipped_idle_reclaim_result(skip_reason);
        state.idle_reclaim.record_result(result.clone());
        return result;
    }
    let sqlite = if config.sqlite_shrink_memory {
        let _admission = state.idle_reclaim.start_backend().await;
        if let Some(skip_reason) = idle_reclaim_backend_skip_reason(state) {
            let result = skipped_idle_reclaim_result(skip_reason);
            state.idle_reclaim.record_result(result.clone());
            return result;
        }
        maybe_run_idle_reclaim_before_backend_call_hook(state);
        let state = Arc::clone(state);
        tokio::task::spawn_blocking(move || shrink_sqlite_stores(&state))
            .await
            .unwrap_or_else(|error| {
                failed_backend_result(format!("join SQLite shrink task: {error}"))
            })
    } else {
        IdleReclaimBackendResult::disabled()
    };
    let mut allocator_skip_reason = None;
    if config.malloc_trim {
        allocator_skip_reason = idle_reclaim_backend_skip_reason(state);
    }
    let allocator = if config.malloc_trim {
        if let Some(_skip_reason) = allocator_skip_reason.as_ref() {
            IdleReclaimBackendResult::skipped()
        } else {
            let _admission = state.idle_reclaim.start_backend().await;
            allocator_skip_reason = idle_reclaim_backend_skip_reason(state);
            if allocator_skip_reason.is_some() {
                IdleReclaimBackendResult::skipped()
            } else {
                maybe_run_idle_reclaim_before_backend_call_hook(state);
                malloc_trim_backend_result()
            }
        }
    } else {
        IdleReclaimBackendResult::disabled()
    };
    let status = reclaim_cycle_status(&sqlite, &allocator);
    let skip_reason = allocator_skip_reason
        .or_else(|| (status == "skipped").then(|| "all_backends_disabled".to_string()));
    let result = IdleReclaimCycleResult {
        attempted_at_unix_ms,
        finished_at_unix_ms: now_unix_millis(),
        status,
        skip_reason,
        sqlite,
        allocator,
    };
    state.idle_reclaim.record_result(result.clone());
    result
}

fn idle_reclaim_backend_skip_reason(state: &SharedState) -> Option<String> {
    idle_reclaim_gate_with_running(state, daemon_resource_snapshots(state), false)
        .health
        .skip_reason
}

#[cfg(test)]
fn maybe_run_idle_reclaim_before_backend_hook(state: &SharedState) {
    let hook = state
        .idle_reclaim_before_backend_hook
        .lock()
        .ok()
        .and_then(|mut hook| hook.take());
    if let Some(mut hook) = hook {
        hook(state);
    }
}

#[cfg(not(test))]
fn maybe_run_idle_reclaim_before_backend_hook(_state: &SharedState) {}

#[cfg(test)]
fn maybe_run_idle_reclaim_before_backend_call_hook(state: &SharedState) {
    let hook = state
        .idle_reclaim_before_backend_call_hook
        .lock()
        .ok()
        .and_then(|mut hook| hook.take());
    if let Some(mut hook) = hook {
        hook(state);
    }
}

#[cfg(not(test))]
fn maybe_run_idle_reclaim_before_backend_call_hook(_state: &SharedState) {}

struct IdleReclaimRunLease {
    runtime: Arc<IdleReclaimRuntime>,
}

impl Drop for IdleReclaimRunLease {
    fn drop(&mut self) {
        self.runtime.running.store(false, Ordering::Release);
    }
}

fn skipped_idle_reclaim_result(skip_reason: String) -> IdleReclaimCycleResult {
    let now = now_unix_millis();
    IdleReclaimCycleResult {
        attempted_at_unix_ms: now,
        finished_at_unix_ms: now,
        status: "skipped".into(),
        skip_reason: Some(skip_reason),
        sqlite: IdleReclaimBackendResult::skipped(),
        allocator: IdleReclaimBackendResult::skipped(),
    }
}

fn reclaim_cycle_status(
    sqlite: &IdleReclaimBackendResult,
    allocator: &IdleReclaimBackendResult,
) -> String {
    let attempted = sqlite.attempted || allocator.attempted;
    let failures = sqlite.failure_count.saturating_add(allocator.failure_count);
    let successes = sqlite.success_count.saturating_add(allocator.success_count);
    if !attempted {
        "skipped".into()
    } else if failures == 0 {
        "succeeded".into()
    } else if successes > 0 {
        "partial_failure".into()
    } else {
        "failed".into()
    }
}

fn idle_reclaim_result_attempted(result: &IdleReclaimCycleResult) -> bool {
    result.sqlite.attempted || result.allocator.attempted
}

fn shrink_sqlite_stores(state: &SharedState) -> IdleReclaimBackendResult {
    let mut result = IdleReclaimBackendResult {
        status: "succeeded".into(),
        attempted: true,
        success_count: 0,
        failure_count: 0,
        last_error: None,
    };
    match state.pipeline.lock() {
        Ok(pipeline) => match pipeline.as_ref() {
            Some(pipeline) => {
                record_sqlite_shrink_result("pipeline", pipeline.store(), &mut result)
            }
            None => record_sqlite_shrink_error("pipeline", "pipeline busy", &mut result),
        },
        Err(error) => record_sqlite_shrink_error("pipeline", &error.to_string(), &mut result),
    }
    match state.task_store.lock() {
        Ok(store) => record_sqlite_shrink_result("task_store", &store, &mut result),
        Err(error) => record_sqlite_shrink_error("task_store", &error.to_string(), &mut result),
    }
    if result.failure_count > 0 {
        result.status = if result.success_count > 0 {
            "partial_failure".into()
        } else {
            "failed".into()
        };
    }
    result
}

fn record_sqlite_shrink_result(name: &str, store: &Store, result: &mut IdleReclaimBackendResult) {
    match store.shrink_memory() {
        Ok(()) => result.success_count = result.success_count.saturating_add(1),
        Err(error) => record_sqlite_shrink_error(name, &error.to_string(), result),
    }
}

fn record_sqlite_shrink_error(name: &str, error: &str, result: &mut IdleReclaimBackendResult) {
    result.failure_count = result.failure_count.saturating_add(1);
    result.last_error = Some(bounded_error(&format!("{name}: {error}")));
}

#[cfg(all(target_os = "linux", target_env = "gnu"))]
fn malloc_trim_backend_result() -> IdleReclaimBackendResult {
    // SAFETY: `malloc_trim` is a process-wide glibc allocator maintenance call.
    // Passing pad=0 provides no pointers or Rust-managed references to C, so the
    // unsafe boundary is limited to trusting glibc's ABI for this platform cfg.
    let released = unsafe { libc::malloc_trim(0) };
    IdleReclaimBackendResult {
        status: if released == 0 {
            "succeeded_no_release".into()
        } else {
            "succeeded".into()
        },
        attempted: true,
        success_count: 1,
        failure_count: 0,
        last_error: None,
    }
}

#[cfg(not(all(target_os = "linux", target_env = "gnu")))]
fn malloc_trim_backend_result() -> IdleReclaimBackendResult {
    IdleReclaimBackendResult {
        status: "unsupported".into(),
        attempted: false,
        success_count: 0,
        failure_count: 0,
        last_error: None,
    }
}

fn failed_backend_result(error: String) -> IdleReclaimBackendResult {
    IdleReclaimBackendResult {
        status: "failed".into(),
        attempted: true,
        success_count: 0,
        failure_count: 1,
        last_error: Some(bounded_error(&error)),
    }
}

fn daemon_resources(config: &DaemonResourceConfig) -> DaemonResources {
    let config = config.bounded();
    let registry = global_resource_registry();
    let resources = DaemonResources {
        sqlite_writer: registry.resource(
            "sqlite_writer",
            "sqlite_write",
            sqlite_writer_resource_limits(&config),
        ),
        sqlite_reader: registry.resource(
            "sqlite_reader",
            "sqlite_read",
            resource_limits(
                config.sqlite_reader_concurrency,
                config.sqlite_reader_queue_capacity,
                config.sqlite_reader_queue_timeout_seconds,
            ),
        ),
        vector_search: registry.resource(
            "vector_search",
            "vector_search",
            resource_limits(
                config.vector_search_concurrency,
                config.vector_search_queue_capacity,
                config.vector_search_queue_timeout_seconds,
            ),
        ),
        cpu_worker: registry.resource(
            "cpu_worker",
            "cpu",
            resource_limits(
                config.cpu_worker_concurrency,
                config.cpu_worker_queue_capacity,
                config.cpu_worker_queue_timeout_seconds,
            ),
        ),
        index_publish: registry.resource(
            "index_publish",
            "index_publish",
            resource_limits(
                config.index_publish_concurrency,
                config.index_publish_queue_capacity,
                config.index_publish_queue_timeout_seconds,
            ),
        ),
        qdrant_upsert: registry.resource(
            "qdrant_upsert",
            "qdrant_upsert",
            resource_limits(
                config.qdrant_upsert_concurrency,
                config.qdrant_upsert_queue_capacity,
                config.qdrant_upsert_queue_timeout_seconds,
            ),
        ),
    };
    configure_daemon_resources(&resources, &config);
    resources
}

fn configure_daemon_resources(resources: &DaemonResources, config: &DaemonResourceConfig) {
    let config = config.bounded();
    resources
        .sqlite_writer
        .configure(sqlite_writer_resource_limits(&config));
    resources.sqlite_reader.configure(resource_limits(
        config.sqlite_reader_concurrency,
        config.sqlite_reader_queue_capacity,
        config.sqlite_reader_queue_timeout_seconds,
    ));
    resources.vector_search.configure(resource_limits(
        config.vector_search_concurrency,
        config.vector_search_queue_capacity,
        config.vector_search_queue_timeout_seconds,
    ));
    resources.cpu_worker.configure(resource_limits(
        config.cpu_worker_concurrency,
        config.cpu_worker_queue_capacity,
        config.cpu_worker_queue_timeout_seconds,
    ));
    resources.index_publish.configure(resource_limits(
        config.index_publish_concurrency,
        config.index_publish_queue_capacity,
        config.index_publish_queue_timeout_seconds,
    ));
    resources.qdrant_upsert.configure(resource_limits(
        config.qdrant_upsert_concurrency,
        config.qdrant_upsert_queue_capacity,
        config.qdrant_upsert_queue_timeout_seconds,
    ));
}

fn resource_limits(
    capacity: usize,
    queue_capacity: usize,
    queue_timeout_seconds: u64,
) -> ResourceLimitConfig {
    ResourceLimitConfig {
        capacity,
        queue_capacity,
        queue_timeout: Duration::from_secs(queue_timeout_seconds),
    }
    .bounded()
}

fn sqlite_writer_resource_limits(config: &DaemonResourceConfig) -> ResourceLimitConfig {
    resource_limits(
        SQLITE_WRITER_ACTIVE_CAPACITY,
        config.sqlite_writer_queue_capacity,
        config.sqlite_writer_queue_timeout_seconds,
    )
}

fn daemon_resource_snapshots(state: &SharedState) -> Vec<ResourceQueueSnapshot> {
    let mut snapshots = vec![
        state.resources.sqlite_writer.snapshot(),
        state.resources.sqlite_reader.snapshot(),
        state.resources.vector_search.snapshot(),
        state.resources.cpu_worker.snapshot(),
        state.resources.index_publish.snapshot(),
    ];
    if runtime_config_snapshot(state)
        .map(|runtime| runtime.config.qdrant.enabled)
        .unwrap_or(false)
    {
        snapshots.push(state.resources.qdrant_upsert.snapshot());
    }
    snapshots.extend(model_endpoint_resource_snapshots());
    snapshots
}

fn readiness_snapshot(state: &SharedState) -> ReadinessHealth {
    state
        .readiness
        .read()
        .map(|readiness| readiness.clone())
        .unwrap_or_else(|error| {
            ReadinessHealth::degraded(
                "readiness_lock",
                Some(format!("readiness lock poisoned: {error}")),
            )
        })
}

fn set_readiness(state: &SharedState, readiness: ReadinessHealth) {
    match state.readiness.write() {
        Ok(mut current) => {
            *current = readiness;
        }
        Err(error) => {
            tracing::error!(error = %error, "failed to update daemon readiness");
        }
    }
}

fn retrieval_readiness_gate(state: &SharedState) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let readiness = readiness_snapshot(state);
    if readiness.retrieval_ready {
        return Ok(());
    }
    let message = retrieval_not_ready_message(&readiness);
    Err((
        StatusCode::SERVICE_UNAVAILABLE,
        Json(ErrorResponse::retrieval_not_ready(message, readiness)),
    ))
}

fn retrieval_not_ready_message(readiness: &ReadinessHealth) -> String {
    let mut message = format!(
        "verbatim daemon is {}; retrieval is not ready (startup_phase={}",
        readiness.state, readiness.startup_phase
    );
    if let Some(reason) = &readiness.degraded_reason {
        message.push_str("; degraded_reason=");
        message.push_str(reason);
    }
    message.push(')');
    message
}

const PIPELINE_BUSY_ERROR: &str = "ingest pipeline is busy with a long-running indexing operation";

fn pipeline_busy_error() -> anyhow::Error {
    anyhow::anyhow!(PIPELINE_BUSY_ERROR)
}

fn is_pipeline_busy_error(error: &anyhow::Error) -> bool {
    error
        .chain()
        .any(|cause| cause.to_string() == PIPELINE_BUSY_ERROR)
}

fn pipeline_ref<'a>(
    pipeline: &'a std::sync::MutexGuard<'_, Option<IngestPipeline>>,
) -> Result<&'a IngestPipeline> {
    pipeline.as_ref().ok_or_else(pipeline_busy_error)
}

fn pipeline_mut<'a>(
    pipeline: &'a mut std::sync::MutexGuard<'_, Option<IngestPipeline>>,
) -> Result<&'a mut IngestPipeline> {
    pipeline.as_mut().ok_or_else(pipeline_busy_error)
}

fn take_pipeline(state: &SharedState) -> Result<IngestPipeline> {
    state
        .pipeline
        .lock()
        .map_err(|error| anyhow::anyhow!("{error}"))?
        .take()
        .ok_or_else(pipeline_busy_error)
}

fn restore_pipeline(state: &SharedState, pipeline: IngestPipeline) -> Result<()> {
    let mut slot = state
        .pipeline
        .lock()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    if slot.is_some() {
        bail!("ingest pipeline slot was unexpectedly occupied during restore");
    }
    *slot = Some(pipeline);
    Ok(())
}

fn pipeline_access_error(error: anyhow::Error) -> (StatusCode, Json<ErrorResponse>) {
    if is_pipeline_busy_error(&error) {
        err(StatusCode::SERVICE_UNAVAILABLE, error)
    } else {
        err(StatusCode::INTERNAL_SERVER_ERROR, error)
    }
}

struct PipelineLease {
    state: SharedState,
    pipeline: Option<IngestPipeline>,
}

impl PipelineLease {
    fn take(state: SharedState) -> Result<Self> {
        let pipeline = take_pipeline(&state)?;
        Ok(Self {
            state,
            pipeline: Some(pipeline),
        })
    }

    fn restore(mut self) -> Result<()> {
        let pipeline = self.pipeline.take().ok_or_else(pipeline_busy_error)?;
        restore_pipeline(&self.state, pipeline)
    }
}

impl Drop for PipelineLease {
    fn drop(&mut self) {
        let Some(pipeline) = self.pipeline.take() else {
            return;
        };
        if let Err(error) = restore_pipeline(&self.state, pipeline) {
            tracing::error!(
                error = %error,
                "failed to restore ingest pipeline slot while dropping pipeline lease"
            );
        }
    }
}

fn run_with_pipeline<T, F>(state: SharedState, operation: F) -> Result<T>
where
    F: FnOnce(&mut IngestPipeline) -> Result<T>,
{
    let mut lease = PipelineLease::take(state)?;
    let result = {
        let pipeline = lease.pipeline.as_mut().ok_or_else(pipeline_busy_error)?;
        operation(pipeline)
    };
    lease.restore()?;
    result
}

async fn with_exclusive_pipeline<T, F>(state: &SharedState, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&mut IngestPipeline) -> Result<T> + Send + 'static,
{
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || run_with_pipeline(state, operation))
        .await
        .context("join exclusive pipeline task")?
}

async fn with_query_pipeline<T, F>(state: &SharedState, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&mut IngestPipeline) -> Result<T> + Send + 'static,
{
    let permit = state.resources.sqlite_reader.acquire().await?;
    let config = runtime_config_snapshot(state)?.config;
    let data_dir = state.data_dir.clone();
    tokio::task::spawn_blocking(move || {
        let mut pipeline = IngestPipeline::open_readonly(&config, &data_dir)?;
        drop(permit);
        operation(&mut pipeline)
    })
    .await
    .context("join read-only query pipeline task")?
}

fn with_sqlite_reader_permit<T>(
    resource: &Arc<ObservableResource>,
    operation: impl FnOnce() -> Result<T>,
) -> Result<T> {
    let _permit = resource
        .acquire_blocking()
        .context("acquire sqlite reader resource for query read")?;
    operation()
}

async fn with_task_store_read<T, F>(state: &SharedState, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&Store) -> Result<T> + Send + 'static,
{
    let permit = state.resources.sqlite_reader.acquire().await?;
    let db_path = state.data_dir.join("verbatim.db");
    let durability_profile = runtime_config_snapshot(state)?.config.store.durability;
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let store =
            Store::open_existing_readonly_with_durability_profile(&db_path, durability_profile)?;
        operation(&store)
    })
    .await
    .context("join sqlite read resource task")?
}

fn initial_index_status_cache(pipeline: &IngestPipeline) -> Option<IndexStatusResponse> {
    match pipeline.index_status() {
        Ok(response) => Some(response),
        Err(error) => {
            tracing::warn!(error = %error, "failed to initialize index status cache");
            None
        }
    }
}

fn update_index_status_cache(state: &SharedState, response: &IndexStatusResponse) -> Result<()> {
    let mut cache = state
        .index_status_cache
        .write()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    *cache = Some(response.clone());
    Ok(())
}

fn cached_index_status(state: &SharedState) -> Result<Option<IndexStatusResponse>> {
    let cache = state
        .index_status_cache
        .read()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    Ok(cache.clone())
}

fn cached_index_status_for_busy_pipeline(state: &SharedState) -> Result<IndexStatusResponse> {
    let mut response = cached_index_status(state)?
        .with_context(|| "index status is unavailable until the first live status snapshot")?;
    response.messages.push(
        "Serving last-known index status because the ingest pipeline is busy with a long-running indexing operation."
            .to_string(),
    );
    Ok(response)
}

async fn with_task_store_write<T, F>(state: &SharedState, operation: F) -> Result<T>
where
    T: Send + 'static,
    F: FnOnce(&mut Store) -> Result<T> + Send + 'static,
{
    let permit = state.resources.sqlite_writer.acquire().await?;
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let _permit = permit;
        let mut store = state
            .task_store
            .lock()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        operation(&mut store)
    })
    .await
    .context("join sqlite write resource task")?
}

fn unix_timestamp_string() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .to_string()
}

fn initial_reload_metadata(config_path: &FsPath) -> ConfigReloadMetadata {
    ConfigReloadMetadata {
        active_config_path: config_path.display().to_string(),
        loaded_at: unix_timestamp_string(),
        last_reload_at: None,
        last_reload_error: None,
        last_applied_reload_safe_keys: Vec::new(),
        last_restart_required_keys: Vec::new(),
    }
}

fn bounded_config_reload_error(error: &str) -> String {
    error.chars().take(CONFIG_RELOAD_ERROR_MAX_CHARS).collect()
}

fn safe_config_reload_error(error: &str) -> String {
    let sanitized_error = sanitize_text(error);
    let safe_lines = sanitized_error
        .lines()
        .filter(|line| {
            let trimmed = line.trim_start();
            !trimmed.starts_with('|')
                && !trimmed
                    .split_once(" | ")
                    .is_some_and(|(prefix, _)| prefix.chars().all(|ch| ch.is_ascii_digit()))
        })
        .collect::<Vec<_>>();
    bounded_config_reload_error(&safe_lines.join(" "))
}

fn runtime_config_snapshot(state: &SharedState) -> Result<RuntimeConfigState> {
    state
        .runtime_config
        .read()
        .map_err(|error| anyhow::anyhow!("{error}"))
        .map(|guard| guard.clone())
}

fn set_collection_watcher_sender(state: &SharedState, tx: mpsc::Sender<CollectionWatcherCommand>) {
    match state.collection_watcher.tx.lock() {
        Ok(mut guard) => {
            *guard = Some(tx);
        }
        Err(error) => {
            tracing::warn!(error = %error, "failed to install collection watcher command sender");
        }
    }
}

fn send_collection_watcher_command(state: &SharedState, command: CollectionWatcherCommand) -> bool {
    let tx = match state.collection_watcher.tx.lock() {
        Ok(guard) => guard.clone(),
        Err(error) => {
            tracing::warn!(error = %error, "failed to lock collection watcher command sender");
            return false;
        }
    };
    if let Some(tx) = tx {
        if let Err(error) = tx.try_send(command) {
            tracing::warn!(error = %error, "collection watcher command queue is full");
            return false;
        }
        return true;
    }
    false
}

fn queue_idle_exit_collection_watcher_resync_if_enabled(state: &SharedState) {
    let config = idle_exit_config(state);
    if !config.enabled || !config.allow_with_collection_watcher {
        return;
    }
    if state
        .idle_exit
        .watcher_resync_requested
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .is_err()
    {
        return;
    }
    if !send_collection_watcher_command(state, CollectionWatcherCommand::ResyncActive) {
        tracing::warn!(
            "collection watcher resync could not be queued; keeping idle exit blocked until resync succeeds or configuration changes"
        );
    }
}

fn update_collection_watcher_status<F>(state: &SharedState, collection_name: &str, update: F)
where
    F: FnOnce(&mut CollectionWatcherStatusState),
{
    match state.collection_watcher.statuses.lock() {
        Ok(mut statuses) => {
            let status = statuses.entry(collection_name.to_string()).or_default();
            update(status);
        }
        Err(error) => {
            tracing::warn!(
                collection = %collection_name,
                error = %error,
                "failed to update collection watcher status"
            );
        }
    }
}

fn record_collection_watcher_error(
    state: &SharedState,
    collection_name: &str,
    error: impl std::fmt::Display,
) {
    let error = bounded_collection_watcher_error(&error.to_string());
    update_collection_watcher_status(state, collection_name, |status| {
        status.last_error = Some(error);
    });
}

fn bounded_collection_watcher_error(error: &str) -> String {
    sanitize_text(error)
        .chars()
        .take(COLLECTION_WATCHER_STATUS_ERROR_MAX_CHARS)
        .collect()
}

fn collection_watcher_status_from_parts(
    collection: &CollectionRecord,
    state: Option<CollectionWatcherStatusState>,
) -> CollectionWatcherStatus {
    let state = state.unwrap_or_default();
    CollectionWatcherStatus {
        collection_name: collection.name.clone(),
        watch_enabled: collection.watch_enabled,
        auto_index_enabled: collection.auto_index_enabled,
        active: state.active,
        ignored_by_config: state.ignored_by_config,
        watched_root_count: state.watched_root_count,
        pending_event_count: state.pending_event_count,
        last_event_at: state.last_event_at,
        last_sync_at: state.last_sync_at,
        last_error: state.last_error,
        last_added: state.last_added,
        last_removed: state.last_removed,
        last_unchanged: state.last_unchanged,
        last_task_id: state.last_task_id,
    }
}

#[derive(Deserialize)]
struct IngestQuery {
    #[serde(default)]
    force: bool,
    #[serde(default)]
    embedding_profile_id: Option<String>,
    #[serde(default)]
    vectors_only: bool,
}

#[derive(Deserialize)]
struct TaskEventsQuery {
    after: Option<i64>,
    limit: Option<usize>,
}

#[derive(Debug, Clone, Deserialize)]
struct PersistedIngestRequest {
    ingest_request_version: Option<u64>,
    #[serde(default)]
    operation: Option<String>,
    #[serde(default)]
    source_id: Option<String>,
    #[serde(default)]
    source_hash: Option<String>,
    #[serde(default)]
    force: bool,
    #[serde(default)]
    embedding_profile_id: Option<String>,
    #[serde(default)]
    vectors_only: bool,
    #[serde(default)]
    queue_claimable: Option<bool>,
    #[serde(default)]
    ingest_batch_id: Option<String>,
}

#[derive(Debug, Clone)]
struct IndexingTaskControls {
    source_id: Option<String>,
    force: bool,
    embedding_profile_id: Option<String>,
    vectors_only: bool,
    ingest_batch_id: Option<String>,
}

#[derive(Debug, Clone)]
struct ResumeTaskPlan {
    task_id: TaskId,
    operation: ResumableIngestOperation,
    controls: IndexingTaskControls,
    queue_claimable: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ResumableIngestOperation {
    Ingest,
    Reindex,
}

impl ResumableIngestOperation {
    fn as_str(self) -> &'static str {
        match self {
            Self::Ingest => "ingest",
            Self::Reindex => "reindex",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum TaskStartOutcome {
    Started,
    BlockedByRunningIngest,
    NotQueued,
}

#[derive(Debug, Clone)]
enum ClaimedIngestWork {
    Single(Box<verbatim_core::task::TaskSummary>),
    SourceBatch(Vec<verbatim_core::task::TaskSummary>),
}

#[derive(Debug, Clone)]
struct IngestBatchExpansionCandidate {
    task_id: TaskId,
    force: bool,
    embedding_profile_id: Option<String>,
    vectors_only: bool,
}

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health(State(state): State<SharedState>) -> Json<HealthResponse> {
    let resources = daemon_resource_snapshots(&state);
    let idle_reclaim = idle_reclaim_gate(&state, resources.clone()).health;
    let idle_exit = idle_exit_health_gate(&state, resources.clone()).health;
    let sqlite_durability = sqlite_durability_ops::health_durability_status(&state);
    Json(HealthResponse {
        status: "ok".into(),
        readiness: readiness_snapshot(&state),
        memory_budget: state.memory_budget.snapshot(),
        resources,
        idle_reclaim: Some(idle_reclaim),
        idle_exit: Some(idle_exit),
        sqlite_durability,
    })
}

async fn get_config(
    State(state): State<SharedState>,
) -> Result<Json<ConfigResponse>, (StatusCode, Json<ErrorResponse>)> {
    let snapshot = runtime_config_snapshot(&state)
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(Json(ConfigResponse {
        config: snapshot.config.redacted_json(),
        reload: snapshot.reload,
    }))
}

async fn index_gc(
    State(state): State<SharedState>,
    Json(req): Json<IndexGcRequest>,
) -> Result<Json<IndexGcResponse>, (StatusCode, Json<ErrorResponse>)> {
    let config = runtime_config_snapshot(&state)
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?
        .config;
    let policy_config = config.index_gc;
    let policy = policy_config.policy();
    let state = Arc::clone(&state);
    let data_dir = state.data_dir.clone();
    let resources = state.resources.clone();
    let dry_run = req.dry_run;
    let (plan, apply) = tokio::task::spawn_blocking(move || {
        run_with_pipeline(state, move |pipeline| {
            if dry_run {
                let plan = plan_index_gc(&data_dir, pipeline.store(), policy)?;
                Ok::<_, anyhow::Error>((plan, IndexGcApplyReport::default()))
            } else {
                let index_publish_permit = resources.index_publish.acquire_blocking()?;
                let result = apply_index_gc(&data_dir, pipeline.store(), policy);
                drop(index_publish_permit);
                result
            }
        })
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(pipeline_access_error)?;
    Ok(Json(IndexGcResponse {
        dry_run,
        policy: policy_config,
        plan,
        apply,
    }))
}

async fn index_status(
    State(state): State<SharedState>,
) -> Result<Json<IndexStatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let config = runtime_config_snapshot(&state)
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?
        .config;
    refresh_live_embedding_profile_capabilities(
        &state,
        config.embedding.enabled,
        &config.embedding.profile_id,
    )
    .await
    .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    let state_for_cache = Arc::clone(&state);
    match with_exclusive_pipeline(&state, move |pipeline| pipeline.index_status()).await {
        Ok(response) => {
            update_index_status_cache(&state_for_cache, &response)
                .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?;
            Ok(Json(response))
        }
        Err(error) if is_pipeline_busy_error(&error) => {
            cached_index_status_for_busy_pipeline(&state_for_cache)
                .map(Json)
                .map_err(|error| err(StatusCode::SERVICE_UNAVAILABLE, error))
        }
        Err(error) => Err(err(StatusCode::INTERNAL_SERVER_ERROR, error)),
    }
}

async fn index_delete_profile(
    State(state): State<SharedState>,
    Json(req): Json<IndexProfileDeleteRequest>,
) -> Result<Json<IndexProfileDeleteResponse>, (StatusCode, Json<ErrorResponse>)> {
    let profile_id = EmbeddingProfileId::new(req.profile_id.clone())
        .map_err(|error| err(StatusCode::BAD_REQUEST, anyhow::anyhow!(error)))?;
    if !req.dry_run && !req.confirm {
        return Err(err(
            StatusCode::BAD_REQUEST,
            anyhow::anyhow!("index profile delete requires confirm=true unless dry_run=true"),
        ));
    }
    if !req.dry_run && !req.allow_active {
        let pipeline = state.pipeline.lock().map_err(|error| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                anyhow::anyhow!("{error}"),
            )
        })?;
        let pipeline =
            pipeline_ref(&pipeline).map_err(|error| err(StatusCode::SERVICE_UNAVAILABLE, error))?;
        if *pipeline.active_embedding_profile_id() == profile_id {
            return Err(err(
                StatusCode::CONFLICT,
                anyhow::anyhow!(
                    "refusing to delete active embedding profile {}; pass allow_active=true to clear active profile artifacts",
                    profile_id
                ),
            ));
        }
    }

    let state = Arc::clone(&state);
    let dry_run = req.dry_run;
    let allow_active = req.allow_active;
    let (plan, apply) = tokio::task::spawn_blocking(move || {
        run_with_pipeline(state, move |pipeline| {
            if dry_run {
                let plan = pipeline.plan_embedding_profile_delete(&profile_id)?;
                Ok::<_, anyhow::Error>((
                    plan,
                    verbatim_core::index_profile_delete::IndexProfileDeleteApplyReport::default(),
                ))
            } else {
                pipeline.delete_embedding_profile_index_data(&profile_id, allow_active)
            }
        })
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(pipeline_access_error)?;
    Ok(Json(IndexProfileDeleteResponse {
        dry_run,
        plan,
        apply,
    }))
}

async fn vector_json_cleanup(
    State(state): State<SharedState>,
    Json(req): Json<VectorJsonCleanupRequest>,
) -> Result<Json<VectorJsonCleanupResponse>, (StatusCode, Json<ErrorResponse>)> {
    if !req.dry_run && !req.confirm {
        return Err(err(
            StatusCode::BAD_REQUEST,
            anyhow::anyhow!("vector JSON cleanup requires confirm=true unless dry_run=true"),
        ));
    }

    let state = Arc::clone(&state);
    let dry_run = req.dry_run;
    let report = tokio::task::spawn_blocking(move || {
        run_with_pipeline(state, move |pipeline| {
            if dry_run {
                pipeline.store().vector_json_cleanup_dry_run()
            } else {
                pipeline.store().cleanup_vector_json_payloads()
            }
        })
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(pipeline_access_error)?;
    Ok(Json(VectorJsonCleanupResponse { dry_run, report }))
}

async fn add_source(
    State(state): State<SharedState>,
    Json(req): Json<AddSourceRequest>,
) -> Result<(StatusCode, Json<AddSourceResponse>), (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(&state);
    let result = tokio::task::spawn_blocking(move || {
        let path = PathBuf::from(&req.path);
        run_with_pipeline(state, move |pipeline| pipeline.add_source(&path))
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| {
        if is_pipeline_busy_error(&e) {
            pipeline_access_error(e)
        } else {
            err(StatusCode::BAD_REQUEST, e)
        }
    })?;

    Ok((
        StatusCode::CREATED,
        Json(AddSourceResponse { id: result.0 }),
    ))
}

async fn list_sources(
    State(state): State<SharedState>,
) -> Result<Json<Vec<SourceResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let sources = with_task_store_read(&state, |store| {
        let sources = store
            .list_sources()?
            .into_iter()
            .map(|source| SourceResponse {
                id: source.id.0,
                path: source.path.to_string_lossy().into_owned(),
                status: format!("{:?}", source.status),
                hash: source.hash,
                parser_used: source.parser_used,
                last_ingested_at: source.last_ingested_at,
                diagnostics: None,
            })
            .collect::<Vec<_>>();
        Ok::<_, anyhow::Error>(sources)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(sources))
}

async fn get_source(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<SourceResponse>, (StatusCode, Json<ErrorResponse>)> {
    let id_clone = id.clone();
    let current_ocr_profile = runtime_config_snapshot(&state)
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?
        .config
        .ocr;
    let current_ocr_profile = current_ocr_profile
        .enabled
        .then(|| current_ocr_profile.profile());
    let source = with_task_store_read(&state, move |store| {
        let source = store.get_source(&SourceId(id_clone))?;
        source
            .map(|source| source_response(store, source, current_ocr_profile.as_ref()))
            .transpose()
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    match source {
        Some(s) => Ok(Json(s)),
        None => Err(err(
            StatusCode::NOT_FOUND,
            anyhow::anyhow!("source not found: {id}"),
        )),
    }
}

fn source_response(
    store: &Store,
    source: verbatim_core::types::Source,
    current_ocr_profile: Option<&verbatim_core::types::OcrProfile>,
) -> Result<SourceResponse> {
    let evidence = store.list_evidence_by_source(&source.id)?;
    let image_artifacts = store.list_image_artifacts_by_source(&source.id)?;
    let diagnostics = source_ingest_diagnostics(
        &source.path,
        &evidence,
        &image_artifacts,
        current_ocr_profile,
    );
    Ok(SourceResponse {
        id: source.id.0,
        path: source.path.to_string_lossy().into_owned(),
        status: format!("{:?}", source.status),
        hash: source.hash,
        parser_used: source.parser_used,
        last_ingested_at: source.last_ingested_at,
        diagnostics: Some(diagnostics),
    })
}

fn source_remove_error(source_id: &str, error: anyhow::Error) -> (StatusCode, Json<ErrorResponse>) {
    let status = if is_source_not_found_error(source_id, &error) {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::INTERNAL_SERVER_ERROR
    };
    err(status, error)
}

fn is_source_not_found_error(source_id: &str, error: &anyhow::Error) -> bool {
    let expected = format!("source not found: {source_id}");
    error.chain().any(|cause| cause.to_string() == expected)
}

async fn check_stale(
    State(state): State<SharedState>,
) -> Result<Json<CheckStaleResponse>, (StatusCode, Json<ErrorResponse>)> {
    let config = runtime_config_snapshot(&state)
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?
        .config;
    refresh_live_embedding_profile_capabilities(
        &state,
        config.embedding.enabled,
        &config.embedding.profile_id,
    )
    .await
    .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    let (ids, profile_status) = with_exclusive_pipeline(&state, move |pipeline| {
        let ids = pipeline.check_stale()?;
        let profile_status = pipeline.index_status()?;
        Ok::<_, anyhow::Error>((ids, profile_status))
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(CheckStaleResponse {
        stale: ids.into_iter().map(|id| id.0).collect(),
        profile_status: Some(profile_status),
    }))
}

async fn create_collection(
    State(state): State<SharedState>,
    Json(req): Json<CreateCollectionRequest>,
) -> Result<(StatusCode, Json<CollectionResponse>), (StatusCode, Json<ErrorResponse>)> {
    let response = with_task_store_write(&state, move |store| {
        let collection = store.create_collection(&req.name, &req.ignore_patterns)?;
        collection_response(store, collection)
    })
    .await
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;

    Ok((StatusCode::CREATED, Json(response)))
}

async fn list_collections(
    State(state): State<SharedState>,
) -> Result<Json<Vec<CollectionRecord>>, (StatusCode, Json<ErrorResponse>)> {
    let collections = with_task_store_read(&state, |store| store.list_collections())
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(collections))
}

async fn get_collection(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<CollectionResponse>, (StatusCode, Json<ErrorResponse>)> {
    let name_for_lookup = name.clone();
    let response = with_task_store_read(&state, move |store| {
        let collection = store.get_collection(&name_for_lookup)?;
        collection
            .map(|collection| collection_response(store, collection))
            .transpose()
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    match response {
        Some(response) => Ok(Json(response)),
        None => Err(err(
            StatusCode::NOT_FOUND,
            anyhow::anyhow!("collection not found: {name}"),
        )),
    }
}

async fn delete_collection(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let name_for_error = name.clone();
    let deleted = with_task_store_write(&state, move |store| store.delete_collection(&name))
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    if deleted {
        send_collection_watcher_command(&state, CollectionWatcherCommand::Refresh);
        Ok(StatusCode::NO_CONTENT)
    } else {
        Err(err(
            StatusCode::NOT_FOUND,
            anyhow::anyhow!("collection not found: {name_for_error}"),
        ))
    }
}

async fn add_collection_root(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Json(req): Json<AddCollectionRootRequest>,
) -> Result<Json<AddCollectionRootResponse>, (StatusCode, Json<ErrorResponse>)> {
    let name_for_error = name.clone();
    let response = with_task_store_write(&state, move |store| {
        let path = PathBuf::from(req.path);
        let already_present = store.get_collection_root(&name, &path)?.is_some();
        let root = store.add_collection_root(&name, &path)?;
        let status = store
            .collection_status(&name)?
            .with_context(|| format!("collection not found: {name}"))?;
        Ok(AddCollectionRootResponse {
            collection_name: status.collection.name,
            root,
            root_count: status.root_count,
            member_count: status.member_count,
            added: !already_present,
        })
    })
    .await
    .map_err(|e| collection_error(&name_for_error, e))?;

    send_collection_watcher_command(&state, CollectionWatcherCommand::Refresh);
    Ok(Json(response))
}

async fn sync_collection(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Json(req): Json<CollectionSyncRequest>,
) -> Result<Json<CollectionSyncResponse>, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(&state);
    let name_for_error = name.clone();
    let inputs = req
        .paths
        .into_iter()
        .map(collection_sync_path_input)
        .collect::<Vec<_>>();
    let report = tokio::task::spawn_blocking(move || {
        run_with_pipeline(state, move |pipeline| {
            pipeline.sync_collection(&name, &inputs, req.max_depth)
        })
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| {
        if is_pipeline_busy_error(&e) {
            pipeline_access_error(e)
        } else {
            collection_error(&name_for_error, e)
        }
    })?;

    Ok(Json(CollectionSyncResponse { report }))
}

async fn collection_status(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<CollectionStatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let name_for_lookup = name.clone();
    let status = with_task_store_read(&state, move |store| {
        store.collection_status(&name_for_lookup)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    match status {
        Some(status) => Ok(Json(CollectionStatusResponse { status })),
        None => Err(err(
            StatusCode::NOT_FOUND,
            anyhow::anyhow!("collection not found: {name}"),
        )),
    }
}

async fn list_collection_watcher_statuses(
    State(state): State<SharedState>,
) -> Result<Json<CollectionWatchersStatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let collections = with_task_store_read(&state, |store| store.list_collections())
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let statuses = state
        .collection_watcher
        .statuses
        .lock()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, anyhow::anyhow!("{e}")))?
        .clone();
    let watchers = collections
        .iter()
        .map(|collection| {
            collection_watcher_status_from_parts(
                collection,
                statuses.get(&collection.name).cloned(),
            )
        })
        .collect();
    Ok(Json(CollectionWatchersStatusResponse { watchers }))
}

async fn collection_watcher_status(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<CollectionWatcherResponse>, (StatusCode, Json<ErrorResponse>)> {
    let name_for_lookup = name.clone();
    let collection =
        with_task_store_read(&state, move |store| store.get_collection(&name_for_lookup))
            .await
            .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let Some(collection) = collection else {
        return Err(err(
            StatusCode::NOT_FOUND,
            anyhow::anyhow!("collection not found: {name}"),
        ));
    };
    let status = state
        .collection_watcher
        .statuses
        .lock()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, anyhow::anyhow!("{e}")))?
        .get(&collection.name)
        .cloned();
    let watcher = collection_watcher_status_from_parts(&collection, status);
    Ok(Json(CollectionWatcherResponse {
        collection,
        watcher,
    }))
}

async fn update_collection_watcher(
    State(state): State<SharedState>,
    Path(name): Path<String>,
    Json(req): Json<CollectionWatcherUpdateRequest>,
) -> Result<Json<CollectionWatcherResponse>, (StatusCode, Json<ErrorResponse>)> {
    let name_for_lookup = name.clone();
    let collection = with_task_store_write(&state, move |store| {
        let current = store
            .get_collection(&name_for_lookup)?
            .with_context(|| format!("collection not found: {name_for_lookup}"))?;
        let auto_index_enabled = req.auto_index_enabled.unwrap_or(current.auto_index_enabled);
        store.update_collection_watch_settings(&name_for_lookup, req.enabled, auto_index_enabled)
    })
    .await
    .map_err(|e| collection_error(&name, e))?;
    let Some(collection) = collection else {
        return Err(err(
            StatusCode::NOT_FOUND,
            anyhow::anyhow!("collection not found: {name}"),
        ));
    };
    send_collection_watcher_command(&state, CollectionWatcherCommand::Refresh);
    let status = state
        .collection_watcher
        .statuses
        .lock()
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, anyhow::anyhow!("{e}")))?
        .get(&collection.name)
        .cloned();
    let watcher = collection_watcher_status_from_parts(&collection, status);
    Ok(Json(CollectionWatcherResponse {
        collection,
        watcher,
    }))
}

fn collection_response(store: &Store, collection: CollectionRecord) -> Result<CollectionResponse> {
    let roots = store.list_collection_roots(&collection.name)?;
    let members = store.list_collection_members(&collection.name)?;
    Ok(CollectionResponse {
        collection,
        roots,
        members,
    })
}

fn collection_sync_path_input(request: CollectionSyncPathRequest) -> CollectionSyncPathInput {
    CollectionSyncPathInput {
        path: PathBuf::from(request.path),
        logical_path: request.logical_path,
    }
}

fn collection_error(
    collection_name: &str,
    error: anyhow::Error,
) -> (StatusCode, Json<ErrorResponse>) {
    let status = if is_collection_not_found_error(collection_name, &error) {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::BAD_REQUEST
    };
    err(status, error)
}

fn is_collection_not_found_error(collection_name: &str, error: &anyhow::Error) -> bool {
    let expected = format!("collection not found: {collection_name}");
    error.chain().any(|cause| cause.to_string() == expected)
}

async fn create_persisted_task(
    state: &SharedState,
    kind: TaskKind,
    request: serde_json::Value,
) -> Result<TaskId, (StatusCode, Json<ErrorResponse>)> {
    create_persisted_task_with_id(state, TaskId::new(), kind, request).await
}

async fn create_persisted_task_with_id(
    state: &SharedState,
    task_id: TaskId,
    kind: TaskKind,
    request: serde_json::Value,
) -> Result<TaskId, (StatusCode, Json<ErrorResponse>)> {
    with_task_store_write(state, move |store| {
        let task = store.create_task(&task_id, kind, &request)?;
        let payload = queued_event_payload(store, task)?;
        store.insert_task_event(&task_id, "queued", "task queued", &payload)?;
        Ok(task_id)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))
}

async fn create_background_ingest_batch(
    state: &SharedState,
    req: TaskIngestRequest,
) -> Result<TaskId, (StatusCode, Json<ErrorResponse>)> {
    let parent_id = TaskId::new();
    let parent_id_for_task = parent_id.clone();
    with_task_store_write(state, move |store| {
        let parent_request = ingest_task_request_metadata_with_queue_claim_and_batch(
            None,
            req.force,
            req.embedding_profile_id.as_deref(),
            req.vectors_only,
            false,
            Some(&parent_id_for_task.0),
        );
        let parent = store.create_task(&parent_id_for_task, TaskKind::Ingest, &parent_request)?;
        let payload = queued_event_payload(store, parent)?;
        store.insert_task_event(&parent_id_for_task, "queued", "task queued", &payload)?;

        Ok(parent_id_for_task)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))
}

#[derive(Debug, Clone)]
struct BackgroundIngestSourceCandidate {
    source_id: SourceId,
    source_hash: Option<String>,
}

#[derive(Debug, Clone, Default)]
struct BackgroundIngestBatchExpansion {
    sources: Vec<BackgroundIngestSourceCandidate>,
    skipped_missing_sources: Vec<SourceId>,
}

async fn source_hash_for_ingest_task_metadata(
    state: &SharedState,
    source_id: &str,
) -> Result<Option<String>, (StatusCode, Json<ErrorResponse>)> {
    let source_id = SourceId(source_id.to_string());
    with_query_pipeline(state, move |pipeline| {
        Ok(pipeline.source_ingest_snapshot(&source_id)?.current_hash)
    })
    .await
    .map_err(pipeline_access_error)
}

fn ingest_task_request_metadata_with_source_hash(
    mut request: serde_json::Value,
    source_hash: Option<&str>,
) -> serde_json::Value {
    if let (Some(source_hash), serde_json::Value::Object(map)) = (source_hash, &mut request) {
        map.insert(
            "source_hash".into(),
            serde_json::Value::String(source_hash.to_string()),
        );
    }
    bounded_json(request)
}

async fn background_ingest_batch_sources(
    state: &SharedState,
    force: bool,
    vectors_only: bool,
) -> Result<BackgroundIngestBatchExpansion> {
    if !force && !vectors_only {
        let config = runtime_config_snapshot(state)?.config;
        refresh_live_embedding_profile_capabilities(
            state,
            config.embedding.enabled,
            &config.embedding.profile_id,
        )
        .await?;
    }
    let runtime = tokio::runtime::Handle::current();
    with_exclusive_pipeline(state, move |pipeline| {
        let skipped_missing_sources = if vectors_only {
            Vec::new()
        } else {
            runtime.block_on(pipeline.remove_missing_sources_for_all_source_ingest(None))?
        };
        if !force && !vectors_only {
            pipeline.check_stale()?;
        }
        let sources = pipeline.store().list_sources()?;
        let mut expansion = BackgroundIngestBatchExpansion {
            skipped_missing_sources,
            ..Default::default()
        };
        for source in sources {
            let source_file_exists = source.path.exists();
            if vectors_only
                || (source_file_exists && (force || source.status != SourceStatus::Indexed))
            {
                let source_hash = if source_file_exists {
                    pipeline.source_ingest_snapshot(&source.id)?.current_hash
                } else {
                    None
                };
                expansion.sources.push(BackgroundIngestSourceCandidate {
                    source_id: source.id,
                    source_hash,
                });
            }
        }
        Ok(expansion)
    })
    .await
}

fn queued_event_payload(
    store: &Store,
    task: verbatim_core::task::TaskSummary,
) -> Result<serde_json::Value> {
    let task = with_queue_details(store, task)?;
    Ok(serde_json::json!({
        "kind": task.kind.as_str(),
        "queue_position": task.queue_position,
        "blocking_reason": task.blocking_reason,
    }))
}

fn with_queue_details(
    store: &Store,
    mut task: verbatim_core::task::TaskSummary,
) -> Result<verbatim_core::task::TaskSummary> {
    if task.status != TaskStatus::Queued {
        return Ok(task);
    }
    let Some(position) = store.queued_task_position(&task.id)? else {
        return Ok(task);
    };
    let running = store.count_running_tasks(task.kind)?;
    task.queue_position = Some(position);
    task.blocking_reason = queued_blocking_reason(task.kind, position, running);
    task.progress = Some(
        task.progress
            .take()
            .unwrap_or_else(|| TaskProgressSnapshot::phase("queued"))
            .with_queue(
                position,
                (running > 0).then(|| task.kind.as_str().to_string()),
                task.blocking_reason.clone(),
            )
            .with_recent_status("queued"),
    );
    Ok(task)
}

fn queued_blocking_reason(kind: TaskKind, position: usize, running: usize) -> Option<String> {
    if position > 1 {
        return Some(format!(
            "waiting for {} queued {} task(s) ahead",
            position - 1,
            kind.as_str()
        ));
    }
    if running > 0 {
        return Some(format!(
            "waiting for running {} task to finish",
            kind.as_str()
        ));
    }
    None
}

async fn mark_task_started(
    state: &SharedState,
    task_id: &TaskId,
) -> Result<bool, (StatusCode, Json<ErrorResponse>)> {
    let task_id = task_id.clone();
    with_task_store_write(state, move |store| {
        let started = store.start_task(&task_id)?;
        if !started {
            return Ok(false);
        }
        store.insert_task_event(&task_id, "started", "task started", &serde_json::json!({}))?;
        Ok(true)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))
}

async fn ensure_task_started(
    state: &SharedState,
    task_id: &TaskId,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if mark_task_started(state, task_id).await? {
        return Ok(());
    }
    Err(err(
        StatusCode::CONFLICT,
        anyhow::anyhow!("task was cancelled before it started"),
    ))
}

async fn task_queue_wait_ms(
    state: &SharedState,
    task_id: &TaskId,
) -> Result<u64, (StatusCode, Json<ErrorResponse>)> {
    let task_id = task_id.clone();
    with_task_store_read(state, move |store| {
        let task = store
            .get_task(&task_id)?
            .with_context(|| format!("task not found: {}", task_id.0))?;
        Ok(task
            .started_at
            .as_deref()
            .and_then(|started_at| unix_seconds_delta_ms(&task.created_at, started_at))
            .unwrap_or(0))
    })
    .await
    .map_err(|e| {
        if e.to_string().contains("task not found") {
            err(StatusCode::NOT_FOUND, e)
        } else {
            err(StatusCode::INTERNAL_SERVER_ERROR, e)
        }
    })
}

async fn try_mark_ingest_task_started(
    state: &SharedState,
    task_id: &TaskId,
) -> Result<TaskStartOutcome, (StatusCode, Json<ErrorResponse>)> {
    let state_for_active = Arc::clone(state);
    let task_id = task_id.clone();
    with_task_store_write(state, move |store| {
        let task = store
            .get_task(&task_id)?
            .with_context(|| format!("task not found: {}", task_id.0))?;
        if task.kind != TaskKind::Ingest {
            bail!("task is not an ingest task: {}", task_id.0);
        }
        if task.status != TaskStatus::Queued {
            return Ok(TaskStartOutcome::NotQueued);
        }
        if state_for_active
            .ingest_worker_active
            .load(Ordering::Acquire)
            || store.count_running_tasks(TaskKind::Ingest)? > 0
        {
            return Ok(TaskStartOutcome::BlockedByRunningIngest);
        }
        if !store.start_task_if_no_running(&task_id, TaskKind::Ingest)? {
            return Ok(TaskStartOutcome::BlockedByRunningIngest);
        }
        store.insert_task_event(&task_id, "started", "task started", &serde_json::json!({}))?;
        Ok(TaskStartOutcome::Started)
    })
    .await
    .map_err(|e| {
        if e.to_string().contains("task not found") {
            err(StatusCode::NOT_FOUND, e)
        } else {
            err(StatusCode::INTERNAL_SERVER_ERROR, e)
        }
    })
}

#[cfg(test)]
async fn ensure_ingest_task_started(
    state: &SharedState,
    task_id: &TaskId,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    loop {
        match try_mark_ingest_task_started(state, task_id).await? {
            TaskStartOutcome::Started => return Ok(()),
            TaskStartOutcome::BlockedByRunningIngest => {
                tokio::time::sleep(TASK_WAIT_POLL_INTERVAL).await;
            }
            TaskStartOutcome::NotQueued => {
                return Err(err(
                    StatusCode::CONFLICT,
                    anyhow::anyhow!("task was cancelled or already started before it could start"),
                ));
            }
        }
    }
}

async fn start_foreground_ingest_task(
    state: &SharedState,
    task_id: &TaskId,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    match try_mark_ingest_task_started(state, task_id).await? {
        TaskStartOutcome::Started => Ok(()),
        TaskStartOutcome::BlockedByRunningIngest => Err(err(
            StatusCode::CONFLICT,
            anyhow::anyhow!(
                "ingest queue busy: another ingest task is running; retry later or use `verbatim ingest --background` to queue persistent work"
            ),
        )),
        TaskStartOutcome::NotQueued => Err(err(
            StatusCode::CONFLICT,
            anyhow::anyhow!("ingest task was cancelled or already started before it could start"),
        )),
    }
}

async fn record_task_event(
    state: &SharedState,
    task_id: &TaskId,
    event_type: &'static str,
    message: impl Into<String>,
    payload: serde_json::Value,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let task_id = task_id.clone();
    let message = message.into();
    with_task_store_write(state, move |store| {
        store.insert_task_event(&task_id, event_type, &message, &payload)?;
        Ok(())
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))
}

async fn record_task_span(
    state: &SharedState,
    task_id: &TaskId,
    timing: verbatim_core::task::FinishedPhaseTiming,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let task_id = task_id.clone();
    let verbatim_core::task::FinishedPhaseTiming {
        phase,
        started_at,
        duration_ms,
        metadata,
    } = timing;
    with_task_store_write(state, move |store| {
        let redaction = store
            .get_task(&task_id)?
            .as_ref()
            .map(|task| task_telemetry_redaction(store, task))
            .transpose()?
            .unwrap_or_default();
        let metadata = public_task_telemetry_value(metadata, &redaction);
        store.insert_task_span(&task_id, &phase, &started_at, duration_ms, &metadata)?;
        Ok(())
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))
}

async fn record_retrieve_local_spans(
    state: &SharedState,
    task_id: &TaskId,
    parent_started_at: &str,
    spans: &RetrievalLocalSpansMs,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    for (phase, debug_field, duration_ms) in retrieve_local_span_entries(spans) {
        record_task_span(
            state,
            task_id,
            verbatim_core::task::FinishedPhaseTiming {
                phase: format!("retrieve.local.{phase}"),
                started_at: parent_started_at.to_string(),
                duration_ms,
                metadata: serde_json::json!({
                    "nested": true,
                    "parent_phase": "retrieval",
                    "unit": "ms",
                    "debug_field": debug_field,
                }),
            },
        )
        .await?;
    }
    Ok(())
}

fn retrieve_local_span_entries(
    spans: &RetrievalLocalSpansMs,
) -> Vec<(&'static str, &'static str, u64)> {
    let mut entries = vec![
        ("setup", "setup_ms", spans.setup_ms),
        (
            "query_embedding",
            "query_embedding_ms",
            spans.query_embedding_ms,
        ),
        (
            "dense_vector_search",
            "dense_vector_search_ms",
            spans.dense_vector_search_ms,
        ),
        ("bm25_search", "bm25_search_ms", spans.bm25_search_ms),
        ("rrf_fusion", "rrf_fusion_ms", spans.rrf_fusion_ms),
        (
            "debug_candidate_pack",
            "debug_candidate_pack_ms",
            spans.debug_candidate_pack_ms,
        ),
        ("rerank_total", "rerank_total_ms", spans.rerank_total_ms),
        (
            "result_hydration",
            "result_hydration_ms",
            spans.result_hydration_ms,
        ),
        (
            "graph_expansion",
            "graph_expansion_ms",
            spans.graph_expansion_ms,
        ),
        (
            "final_evidence_pack",
            "final_evidence_pack_ms",
            spans.final_evidence_pack_ms,
        ),
        (
            "display_evidence_pack",
            "display_evidence_pack_ms",
            spans.display_evidence_pack_ms,
        ),
        (
            "response_formatting",
            "response_formatting_ms",
            spans.response_formatting_ms,
        ),
    ];
    if let Some(duration_ms) = spans.canonical_support_embedding_ms {
        entries.push((
            "canonical_support_embedding",
            "canonical_support_embedding_ms",
            duration_ms,
        ));
    }
    if let Some(duration_ms) = spans.vector_queue_wait_ms {
        entries.push(("vector_queue_wait", "vector_queue_wait_ms", duration_ms));
    }
    if let Some(duration_ms) = spans.vector_service_ms {
        entries.push(("vector_service", "vector_service_ms", duration_ms));
    }
    if let Some(duration_ms) = spans.canonical_display_selection_ms {
        entries.push((
            "canonical_display_selection",
            "canonical_display_selection_ms",
            duration_ms,
        ));
    }
    entries
}

fn retrieve_endpoint_summaries(debug: &RetrievalDebug) -> Vec<TaskEndpointSummary> {
    let mut endpoints = Vec::new();
    if let Some(latency_ms) = debug.query_embedding_latency_ms {
        endpoints.push(TaskEndpointSummary::single_call("embedding", latency_ms));
    }
    if let Some(latency_ms) = debug.reranker.latency_ms {
        let endpoint = match debug.reranker.status {
            RetrievalRerankStatus::Fallback => TaskEndpointSummary::failed_call(
                "reranker",
                Some(latency_ms),
                debug
                    .reranker
                    .reason
                    .clone()
                    .unwrap_or_else(|| "rerank fallback".into()),
            ),
            RetrievalRerankStatus::Succeeded
            | RetrievalRerankStatus::Disabled
            | RetrievalRerankStatus::Skipped => {
                TaskEndpointSummary::single_call("reranker", latency_ms)
            }
        };
        endpoints.push(endpoint);
    }
    endpoints
}

fn retrieve_task_profile_from_debug(
    debug: &RetrievalDebug,
    configured_rerank_top_n: usize,
    total_results: usize,
    returned_results: usize,
) -> RetrieveTaskProfile {
    let spans = &debug.local_spans_ms;
    let canonical_selected_count = spans
        .canonical_display_selection_ms
        .map(|_| debug.display_evidence_count);
    RetrieveTaskProfile {
        candidate_counters: debug.candidate_counters,
        dense: RetrieveDenseStageProfile {
            path: debug.dense_vector_path,
            candidate_count: debug
                .candidate_counters
                .returned_k(SpanKind::DenseRetrieval) as usize,
            local_ms: spans.dense_vector_search_ms,
            query_embedding_ms: spans.query_embedding_ms,
            endpoint_latency_ms: debug.query_embedding_latency_ms,
        },
        bm25: RetrieveStageProfile {
            candidate_count: debug
                .candidate_counters
                .returned_k(SpanKind::LexicalRetrieval) as usize,
            local_ms: spans.bm25_search_ms,
        },
        fusion: RetrieveStageProfile {
            candidate_count: debug.candidate_counters.fused() as usize,
            local_ms: spans.rrf_fusion_ms,
        },
        rerank: RetrieveRerankStageProfile {
            status: debug.reranker.status,
            reason: debug.reranker.reason.clone(),
            input_count: debug.reranker.candidate_count,
            configured_top_n: configured_rerank_top_n,
            effective_top_n: debug.reranker.top_n,
            output_count: debug.reranker.scores.len(),
            local_ms: spans.rerank_total_ms,
            endpoint_latency_ms: debug.reranker.latency_ms,
        },
        evidence: RetrieveEvidenceStageProfile {
            result_count: total_results,
            graph_expanded_count: debug.graph_expanded_hits.len(),
            final_count: debug.final_evidence_count,
            display_count: debug.display_evidence_count,
            result_hydration_ms: spans.result_hydration_ms,
            graph_expansion_ms: spans.graph_expansion_ms,
            final_pack_ms: spans.final_evidence_pack_ms,
            display_pack_ms: spans.display_evidence_pack_ms,
        },
        display: RetrieveDisplayStageProfile {
            returned_count: returned_results,
            response_formatting_ms: spans.response_formatting_ms,
            canonical_support_embedding_ms: spans.canonical_support_embedding_ms,
            canonical_display_selection_ms: spans.canonical_display_selection_ms,
            canonical_selected_count,
        },
    }
}

fn retrieve_profile_controls(
    controls: &EffectiveRetrieveControls,
    embedding_profile_id: &EmbeddingProfileId,
    query_scope: &QueryScope,
    debug: &RetrievalDebug,
) -> TaskProfileControls {
    TaskProfileControls {
        retrieval: TaskRetrievalControls {
            dense_top_k: Some(controls.retrieval_config.dense_top_k),
            bm25_top_k: Some(controls.retrieval_config.bm25_top_k),
            rrf_k: Some(controls.retrieval_config.rrf_k),
            fast: controls.fast,
            bypass_cache: controls.bypass_cache,
        },
        rerank: rerank_profile_controls(&controls.rerank_config, debug),
        qdrant: qdrant_profile_controls(&controls.config, debug),
        vector: vector_profile_controls(&controls.config, embedding_profile_id, debug),
        filters: task_filter_controls(query_scope),
        output: TaskOutputControls {
            limit: Some(controls.limit),
            page_size: Some(controls.page_size),
            page: Some(controls.page),
            passage: controls.passage,
            include_locator: controls.include_locator,
            include_debug: controls.include_debug,
            include_debug_packs: controls.include_debug_packs,
            show_retrieval: None,
        },
    }
}

fn ask_profile_controls(
    config: &Config,
    embedding_profile_id: &EmbeddingProfileId,
    query_scope: &QueryScope,
    debug: Option<&RetrievalDebug>,
    show_retrieval: bool,
) -> TaskProfileControls {
    TaskProfileControls {
        retrieval: TaskRetrievalControls {
            dense_top_k: Some(config.retrieval.dense_top_k),
            bm25_top_k: Some(config.retrieval.bm25_top_k),
            rrf_k: Some(config.retrieval.rrf_k),
            fast: false,
            bypass_cache: false,
        },
        rerank: debug
            .map(|debug| rerank_profile_controls(&config.rerank, debug))
            .unwrap_or_else(|| rerank_config_profile_controls(&config.rerank, None)),
        qdrant: debug
            .map(|debug| qdrant_profile_controls(config, debug))
            .unwrap_or_else(|| TaskQdrantControls {
                enabled: config.qdrant.enabled,
                preferred: config.qdrant.prefer_for_search,
                used: false,
            }),
        vector: debug
            .map(|debug| vector_profile_controls(config, embedding_profile_id, debug))
            .unwrap_or_else(|| TaskVectorControls {
                embedding_enabled: config.embedding.enabled,
                embedding_profile_id: Some(bounded_profile_text(embedding_profile_id.as_str())),
                residency: Some(config.vector_index.residency),
                dense_path: None,
            }),
        filters: task_filter_controls(query_scope),
        output: TaskOutputControls {
            limit: None,
            page_size: None,
            page: None,
            passage: false,
            include_locator: false,
            include_debug: show_retrieval,
            include_debug_packs: show_retrieval,
            show_retrieval: Some(show_retrieval),
        },
    }
}

fn rerank_profile_controls(
    rerank_config: &RerankConfig,
    debug: &RetrievalDebug,
) -> TaskRerankControls {
    rerank_config_profile_controls(rerank_config, debug.reranker.top_n)
}

fn rerank_config_profile_controls(
    rerank_config: &RerankConfig,
    effective_top_n: Option<usize>,
) -> TaskRerankControls {
    TaskRerankControls {
        enabled: rerank_config.enabled,
        configured_top_n: Some(rerank_config.top_n),
        effective_top_n,
        strategy: Some(rerank_config.strategy),
        provider: nonempty_profile_text(&rerank_config.provider),
        model: nonempty_profile_text(&rerank_config.model),
    }
}

fn qdrant_profile_controls(config: &Config, debug: &RetrievalDebug) -> TaskQdrantControls {
    TaskQdrantControls {
        enabled: config.qdrant.enabled,
        preferred: config.qdrant.prefer_for_search,
        used: debug.dense_vector_path == RetrievalDenseVectorPath::Qdrant,
    }
}

fn vector_profile_controls(
    config: &Config,
    embedding_profile_id: &EmbeddingProfileId,
    debug: &RetrievalDebug,
) -> TaskVectorControls {
    TaskVectorControls {
        embedding_enabled: config.embedding.enabled,
        embedding_profile_id: Some(bounded_profile_text(embedding_profile_id.as_str())),
        residency: Some(config.vector_index.residency),
        dense_path: Some(debug.dense_vector_path),
    }
}

fn task_filter_controls(query_scope: &QueryScope) -> TaskFilterControls {
    TaskFilterControls {
        source: TaskSourceFilterControls {
            requested_source_id: query_scope
                .source_id
                .as_ref()
                .map(|source_id| bounded_profile_text(&source_id.0)),
            effective_source_count: query_scope.source_filter.as_ref().map(HashSet::len),
        },
        collection: collection_filter_profile_controls(query_scope.collection_filter.as_ref()),
    }
}

fn collection_filter_profile_controls(
    collection_filter: Option<&CollectionFilterResponse>,
) -> TaskCollectionFilterControls {
    let Some(collection_filter) = collection_filter else {
        return TaskCollectionFilterControls::default();
    };
    let requested = bounded_collection_filter_names(&collection_filter.requested);
    TaskCollectionFilterControls {
        requested_count: requested.0,
        requested_names: requested.1,
        requested_truncated: requested.2,
        require_fresh: collection_filter.requested.require_fresh,
        applied_count: Some(collection_filter.applied.len()),
        union_source_count: Some(collection_filter.union_source_count),
        stale: Some(collection_filter.stale),
        warning_count: Some(collection_filter.warnings.len()),
    }
}

fn bounded_collection_filter_names(filter: &CollectionFilterRequest) -> (usize, Vec<String>, bool) {
    let names = filter
        .collection_ids
        .iter()
        .chain(&filter.names)
        .map(|name| bounded_profile_text(name.trim()))
        .collect::<BTreeSet<_>>();
    let requested_count = names.len();
    let requested_names = names
        .into_iter()
        .take(TASK_PROFILE_COLLECTION_SAMPLE_LIMIT)
        .collect::<Vec<_>>();
    let requested_truncated = requested_count > requested_names.len();
    (requested_count, requested_names, requested_truncated)
}

fn task_resource_profile_from_state(state: &SharedState) -> TaskResourceProfile {
    TaskResourceProfile {
        queues: daemon_resource_snapshots(state)
            .iter()
            .take(TASK_PROFILE_RESOURCE_QUEUE_LIMIT)
            .map(Into::into)
            .collect(),
    }
}

fn nonempty_profile_text(value: &str) -> Option<String> {
    (!value.is_empty()).then(|| bounded_profile_text(value))
}

fn bounded_profile_text(value: &str) -> String {
    let mut output = String::new();
    for (index, ch) in value.chars().enumerate() {
        if index == TASK_PROFILE_STRING_MAX_CHARS {
            output.push_str("...[truncated]");
            return output;
        }
        output.push(ch);
    }
    output
}

fn ask_task_profile_from_telemetry(
    telemetry: &GenerationTelemetry,
    response_formatting_ms: u64,
    answer: &str,
    citation_count: usize,
    retrieval_included: bool,
) -> AskTaskProfile {
    AskTaskProfile {
        generation: ask_generation_stage_profile(&telemetry.generation),
        verification: ask_verification_stage_profile(&telemetry.verification),
        output: AskOutputStageProfile {
            response_formatting_ms,
            answer_chars: answer.chars().count(),
            citation_count,
            retrieval_included,
        },
    }
}

fn ask_generation_stage_profile(telemetry: &GenerationCallTelemetry) -> AskGenerationStageProfile {
    AskGenerationStageProfile {
        status: if telemetry.call_count == 0 {
            AskGenerationStatus::Skipped
        } else if telemetry.error_count > 0 {
            AskGenerationStatus::Failed
        } else {
            AskGenerationStatus::Succeeded
        },
        call_count: telemetry.call_count,
        total_latency_ms: telemetry.total_latency_ms,
        latest_latency_ms: telemetry.latest_latency_ms,
        retry_count: telemetry.retry_count,
        error_count: telemetry.error_count,
        latest_error: telemetry.latest_error.as_deref().map(bounded_error),
    }
}

fn ask_verification_stage_profile(
    telemetry: &verbatim_core::generate::GenerationVerificationTelemetry,
) -> AskVerificationStageProfile {
    AskVerificationStageProfile {
        enabled: telemetry.enabled,
        status: match telemetry.status {
            GenerationVerificationStatus::Disabled => AskVerificationStatus::Disabled,
            GenerationVerificationStatus::Passed => AskVerificationStatus::Passed,
            GenerationVerificationStatus::Revised => AskVerificationStatus::Revised,
            GenerationVerificationStatus::Failed => AskVerificationStatus::Failed,
            GenerationVerificationStatus::Skipped => AskVerificationStatus::Skipped,
        },
        call_count: telemetry.calls.call_count,
        total_latency_ms: telemetry.calls.total_latency_ms,
        latest_latency_ms: telemetry.calls.latest_latency_ms,
        retry_count: telemetry.calls.retry_count,
        error_count: telemetry.calls.error_count,
        latest_error: telemetry.calls.latest_error.as_deref().map(bounded_error),
    }
}

fn ask_endpoint_summaries(telemetry: &GenerationTelemetry) -> Vec<TaskEndpointSummary> {
    let mut endpoints = Vec::new();
    if let Some(endpoint) = call_telemetry_endpoint_summary("chat", &telemetry.generation) {
        endpoints.push(endpoint);
    }
    if let Some(endpoint) =
        call_telemetry_endpoint_summary("verifier", &telemetry.verification.calls)
    {
        endpoints.push(endpoint);
    }
    endpoints
}

fn call_telemetry_endpoint_summary(
    name: &'static str,
    telemetry: &GenerationCallTelemetry,
) -> Option<TaskEndpointSummary> {
    if telemetry.call_count == 0 {
        return None;
    }
    Some(TaskEndpointSummary {
        name: name.into(),
        calls: telemetry.call_count,
        latest_latency_ms: telemetry.latest_latency_ms,
        first_token_latency_ms: None,
        p50_latency_ms: telemetry.latest_latency_ms,
        p95_latency_ms: telemetry.latest_latency_ms,
        latest_error: telemetry.latest_error.as_deref().map(bounded_error),
    })
}

async fn record_task_progress(
    state: &SharedState,
    task_id: &TaskId,
    progress: TaskProgressSnapshot,
) {
    let task_id = task_id.clone();
    let task_id_for_log = task_id.clone();
    let result = with_task_store_write(state, move |store| {
        store.update_task_progress(&task_id, progress)?;
        Ok(())
    })
    .await;

    if let Err(err) = result {
        tracing::warn!(task_id = %task_id_for_log.0, error = %err, "failed to persist task progress");
    }
}

fn record_ingest_task_terminalize_span(
    store: &Store,
    task_id: &TaskId,
    timing: PhaseTiming,
    operation: &'static str,
) {
    let finished = timing.finish(serde_json::json!({
        "operation": operation,
        "task_kind": TaskKind::Ingest.as_str(),
    }));
    if let Err(err) = store.insert_task_span(
        task_id,
        &finished.phase,
        &finished.started_at,
        finished.duration_ms,
        &finished.metadata,
    ) {
        tracing::warn!(
            task_id = %task_id.0,
            error = %err,
            "failed to persist ingest task terminalize span"
        );
    }
}

#[derive(Debug, Clone, Copy)]
struct TaskTerminalizationOutcome {
    task_changed: bool,
    should_wake_ingest_queue: bool,
}

fn mark_idle_reclaim_activity_if_changed(state: &SharedState, changed: bool) {
    if changed {
        state.idle_reclaim.mark_activity();
        state.idle_exit.mark_activity();
    }
}

async fn finish_task_success(
    state: &SharedState,
    task_id: &TaskId,
    result: serde_json::Value,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    finish_task_success_with_optional_profile(state, task_id, result, None).await
}

async fn finish_task_success_with_profile(
    state: &SharedState,
    task_id: &TaskId,
    result: serde_json::Value,
    profile: TaskProfile,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    finish_task_success_with_optional_profile(state, task_id, result, Some(profile)).await
}

async fn finish_task_success_with_optional_profile(
    state: &SharedState,
    task_id: &TaskId,
    result: serde_json::Value,
    profile: Option<TaskProfile>,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let state_for_queue = Arc::clone(state);
    let task_id = task_id.clone();
    let outcome = with_task_store_write(state, move |store| {
        let task = store.get_task(&task_id)?;
        let should_wake_ingest_queue = task
            .as_ref()
            .is_some_and(|task| task.kind == TaskKind::Ingest);
        let terminalize_timing = should_wake_ingest_queue
            .then(|| PhaseTiming::start(IngestTaskStage::TaskTerminalize.as_str()));
        let task_changed = match profile.as_ref() {
            Some(profile) => store.finish_task_success_with_profile(&task_id, &result, profile)?,
            None => store.finish_task_success(&task_id, &result)?,
        };
        if task_changed {
            store.insert_task_event(&task_id, "succeeded", "task succeeded", &result)?;
            if let Some(timing) = terminalize_timing {
                record_ingest_task_terminalize_span(store, &task_id, timing, "finish_task_success");
            }
            finalize_ingest_batch_parent_if_complete(store, task.as_ref())?;
        }
        Ok(TaskTerminalizationOutcome {
            task_changed,
            should_wake_ingest_queue: task_changed && should_wake_ingest_queue,
        })
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    mark_idle_reclaim_activity_if_changed(state, outcome.task_changed);
    if outcome.should_wake_ingest_queue {
        schedule_ingest_queue(state_for_queue);
    }
    Ok(())
}

async fn finish_task_failed(
    state: &SharedState,
    task_id: &TaskId,
    error_message: &str,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    finish_task_failed_with_upstream(state, task_id, error_message, None).await
}

async fn finish_task_failed_from_response(
    state: &SharedState,
    task_id: &TaskId,
    error: &ErrorResponse,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    finish_task_failed_with_upstream(
        state,
        task_id,
        &error.error,
        error.upstream_failure.clone().map(|failure| *failure),
    )
    .await
}

async fn finish_task_failed_with_upstream(
    state: &SharedState,
    task_id: &TaskId,
    error_message: &str,
    upstream_failure: Option<serde_json::Value>,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let state_for_queue = Arc::clone(state);
    let task_id = task_id.clone();
    let error_message = bounded_error(error_message);
    let outcome = with_task_store_write(state, move |store| {
        let task = store.get_task(&task_id)?;
        let upstream_failure = upstream_failure
            .map(|failure| upstream_failure_with_task_context(failure, &task_id, task.as_ref()));
        let should_wake_ingest_queue = task
            .as_ref()
            .is_some_and(|task| task.kind == TaskKind::Ingest);
        let terminalize_timing = should_wake_ingest_queue
            .then(|| PhaseTiming::start(IngestTaskStage::TaskTerminalize.as_str()));
        let resumability = task.as_ref().and_then(|task| {
            task_failure_resumability_metadata(task, Some(&error_message))
                .ok()
                .flatten()
        });
        let task_changed = store.finish_task_failed_with_result(
            &task_id,
            &error_message,
            resumability.as_ref(),
        )?;
        if task_changed {
            let mut payload = serde_json::json!({ "error": error_message });
            if let Some(upstream_failure) = upstream_failure {
                payload["upstream_failure"] = upstream_failure;
            }
            if let Some(resumability) = resumability {
                payload["resumability"] = resumability;
            }
            store.insert_task_event(&task_id, "failed", "task failed", &payload)?;
            if let Some(timing) = terminalize_timing {
                record_ingest_task_terminalize_span(store, &task_id, timing, "finish_task_failed");
            }
            finalize_ingest_batch_parent_if_complete(store, task.as_ref())?;
        }
        Ok(TaskTerminalizationOutcome {
            task_changed,
            should_wake_ingest_queue: task_changed && should_wake_ingest_queue,
        })
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    mark_idle_reclaim_activity_if_changed(state, outcome.task_changed);
    if outcome.should_wake_ingest_queue {
        schedule_ingest_queue(state_for_queue);
    }
    Ok(())
}

fn task_failure_resumability_metadata(
    task: &verbatim_core::task::TaskSummary,
    previous_error: Option<&str>,
) -> Result<Option<serde_json::Value>> {
    let Some(plan) = resumable_task_plan(task)? else {
        return Ok(None);
    };
    Ok(Some(resumability_payload(
        &plan,
        previous_error.or(task.error.as_deref()),
        "failed task can be resumed by task id",
    )))
}

fn resumability_payload(
    plan: &ResumeTaskPlan,
    previous_error: Option<&str>,
    message: &'static str,
) -> serde_json::Value {
    serde_json::json!({
        "resumable": true,
        "message": message,
        "operation": plan.operation.as_str(),
        "resume_command": format!("verbatim task resume {}", plan.task_id.0),
        "queue_claimable": plan.queue_claimable,
        "source_id": plan.controls.source_id.clone(),
        "embedding_profile_id": plan.controls.embedding_profile_id.clone(),
        "vectors_only": plan.controls.vectors_only,
        "force": plan.controls.force,
        "ingest_batch_id": plan.controls.ingest_batch_id.clone(),
        "previous_error": previous_error.map(bounded_error),
    })
}

fn upstream_failure_with_task_context(
    mut upstream_failure: serde_json::Value,
    task_id: &TaskId,
    task: Option<&verbatim_core::task::TaskSummary>,
) -> serde_json::Value {
    let Some(map) = upstream_failure.as_object_mut() else {
        return upstream_failure;
    };
    map.insert(
        "task_id".into(),
        serde_json::Value::String(task_id.0.clone()),
    );
    if let Some(task) = task {
        map.insert(
            "task_kind".into(),
            serde_json::Value::String(task.kind.as_str().into()),
        );
        copy_task_request_field(map, &task.request, "source_id");
        copy_task_request_field(map, &task.request, "embedding_profile_id");
    }
    upstream_failure
}

fn copy_task_request_field(
    map: &mut serde_json::Map<String, serde_json::Value>,
    request: &serde_json::Value,
    field: &'static str,
) {
    if map.contains_key(field) {
        return;
    }
    if let Some(value) = request.get(field) {
        map.insert(field.into(), value.clone());
    }
}

#[derive(Debug, Clone, Default)]
struct TaskTelemetryRedaction {
    private_source_ids: Vec<String>,
}

impl TaskTelemetryRedaction {
    fn from_task(task: &TaskSummary) -> Self {
        let mut redaction = Self::default();
        if let Some(source_id) = task_private_source_id(task) {
            redaction.add_source_id(source_id);
        }
        redaction
    }

    fn add_source_id(&mut self, source_id: impl Into<String>) {
        let source_id = source_id.into();
        if source_id.is_empty() || self.private_source_ids.contains(&source_id) {
            return;
        }
        self.private_source_ids.push(source_id);
        self.private_source_ids
            .sort_by_key(|source_id| std::cmp::Reverse(source_id.len()));
    }

    fn redact_text(&self, text: &str) -> String {
        let mut redacted = text.to_string();
        for source_id in &self.private_source_ids {
            redacted = redacted.replace(source_id, TASK_TELEMETRY_REDACTED);
        }
        redacted
    }
}

fn task_telemetry_redaction(store: &Store, task: &TaskSummary) -> Result<TaskTelemetryRedaction> {
    let mut redaction = TaskTelemetryRedaction::from_task(task);
    if task.kind == TaskKind::Ingest && task_private_source_id(task).is_none() {
        for source in store.list_sources()? {
            redaction.add_source_id(source.id.0);
        }
    }
    Ok(redaction)
}

fn public_task_summary(mut task: TaskSummary, redaction: &TaskTelemetryRedaction) -> TaskSummary {
    task.error = task
        .error
        .map(|error| bounded_error(&redact_task_telemetry_text(&error, redaction)));
    task.request = public_task_telemetry_value(task.request, redaction);
    task.result = task
        .result
        .map(|result| public_task_telemetry_value(result, redaction));
    if let Some(progress) = task.progress.take() {
        task.progress = Some(public_task_progress(progress, redaction));
    }
    task
}

fn public_task_progress(
    progress: TaskProgressSnapshot,
    redaction: &TaskTelemetryRedaction,
) -> TaskProgressSnapshot {
    let mut progress = progress.bounded().with_current_elapsed();
    if let Some(phase) = &mut progress.phase {
        phase.name = public_task_progress_text(&phase.name, redaction);
    }
    for counter in &mut progress.counters {
        counter.name = public_task_progress_text(&counter.name, redaction);
    }
    for endpoint in &mut progress.endpoints {
        endpoint.name = public_task_progress_text(&endpoint.name, redaction);
        endpoint.latest_error = endpoint
            .latest_error
            .as_deref()
            .map(|error| public_task_progress_text(error, redaction));
    }
    if let Some(queue) = &mut progress.queue {
        queue.active_worker_kind = queue
            .active_worker_kind
            .as_deref()
            .map(|worker| public_task_progress_text(worker, redaction));
        queue.blocking_reason = queue
            .blocking_reason
            .as_deref()
            .map(|reason| public_task_progress_text(reason, redaction));
    }
    progress.active_worker_kind = progress
        .active_worker_kind
        .as_deref()
        .map(|worker| public_task_progress_text(worker, redaction));
    progress.wait_reason = progress
        .wait_reason
        .as_deref()
        .map(|reason| public_task_progress_text(reason, redaction));
    progress.recent_status = progress
        .recent_status
        .as_deref()
        .map(|status| public_task_progress_text(status, redaction));
    for resource in &mut progress.resources {
        resource.name = public_task_progress_text(&resource.name, redaction);
        resource.kind = public_task_progress_text(&resource.kind, redaction);
        resource.state = public_task_progress_text(&resource.state, redaction);
    }
    progress
}

fn public_task_progress_text(text: &str, redaction: &TaskTelemetryRedaction) -> String {
    redact_task_telemetry_text(&sanitize_text(text), redaction)
}

fn task_private_source_id(task: &TaskSummary) -> Option<String> {
    task.request
        .get("source_id")
        .and_then(serde_json::Value::as_str)
        .filter(|source_id| !source_id.is_empty())
        .map(str::to_string)
}

fn public_task_event_for_source(
    mut event: TaskEvent,
    redaction: &TaskTelemetryRedaction,
) -> TaskEvent {
    event.message = redact_task_telemetry_text(&event.message, redaction);
    event.payload = public_task_telemetry_value(event.payload, redaction);
    event
}

fn public_task_span(mut span: TaskSpan, redaction: &TaskTelemetryRedaction) -> TaskSpan {
    span.metadata = public_task_telemetry_value(span.metadata, redaction);
    span
}

fn public_task_telemetry_value(
    value: serde_json::Value,
    redaction: &TaskTelemetryRedaction,
) -> serde_json::Value {
    bounded_json(redact_task_telemetry_value(value, redaction))
}

fn redact_task_telemetry_value(
    value: serde_json::Value,
    redaction: &TaskTelemetryRedaction,
) -> serde_json::Value {
    match value {
        serde_json::Value::Object(map) => redact_task_telemetry_object(map, redaction),
        serde_json::Value::Array(items) => serde_json::Value::Array(
            items
                .into_iter()
                .map(|item| redact_task_telemetry_value(item, redaction))
                .collect(),
        ),
        serde_json::Value::String(text) => {
            serde_json::Value::String(redact_task_telemetry_text(&text, redaction))
        }
        other => other,
    }
}

fn redact_task_telemetry_object(
    map: serde_json::Map<String, serde_json::Value>,
    redaction: &TaskTelemetryRedaction,
) -> serde_json::Value {
    let mut output = serde_json::Map::with_capacity(map.len());
    for (key, value) in map {
        let value = if is_private_task_telemetry_key(&key) {
            redacted_task_telemetry_value(value)
        } else {
            redact_task_telemetry_value(value, redaction)
        };
        output.insert(key, value);
    }
    serde_json::Value::Object(output)
}

fn redacted_task_telemetry_value(value: serde_json::Value) -> serde_json::Value {
    match value {
        serde_json::Value::Null => serde_json::Value::Null,
        serde_json::Value::Array(items) => serde_json::json!({
            "redacted": true,
            "count": items.len(),
        }),
        serde_json::Value::Object(map) => serde_json::json!({
            "redacted": true,
            "field_count": map.len(),
        }),
        _ => serde_json::Value::String(TASK_TELEMETRY_REDACTED.into()),
    }
}

fn redact_task_telemetry_text(text: &str, redaction: &TaskTelemetryRedaction) -> String {
    redaction.redact_text(text)
}

fn is_private_task_telemetry_key(key: &str) -> bool {
    let normalized = key.to_ascii_lowercase().replace(['-', '_'], "");
    matches!(
        normalized.as_str(),
        "sourceid"
            | "sourceids"
            | "sourcepath"
            | "sourcepaths"
            | "path"
            | "paths"
            | "pathorurl"
            | "embedding"
            | "embeddings"
            | "vector"
            | "vectors"
            | "embeddingvector"
            | "embeddingvectors"
            | "vectorpayload"
            | "vectorpayloads"
            | "vectorvalue"
            | "vectorvalues"
    ) || normalized.ends_with("sourceid")
        || normalized.ends_with("sourceids")
        || normalized.ends_with("sourcepath")
        || normalized.ends_with("sourcepaths")
        || normalized.ends_with("embeddingvector")
        || normalized.ends_with("embeddingvectors")
        || normalized.ends_with("vectorpayload")
        || normalized.ends_with("vectorpayloads")
        || normalized.ends_with("vectorvalue")
        || normalized.ends_with("vectorvalues")
}

async fn task_summary_response(
    state: &SharedState,
    task_id: TaskId,
) -> Result<TaskSummaryResponse, (StatusCode, Json<ErrorResponse>)> {
    with_task_store_read(state, move |store| {
        let task = store
            .get_task(&task_id)?
            .with_context(|| format!("task not found: {}", task_id.0))?;
        let redaction = task_telemetry_redaction(store, &task)?;
        let task = public_task_summary(with_queue_details(store, task)?, &redaction);
        let spans = store
            .list_task_spans(&task_id)?
            .into_iter()
            .map(|span| public_task_span(span, &redaction))
            .collect();
        Ok(TaskSummaryResponse { task, spans })
    })
    .await
    .map_err(|e| {
        if e.to_string().contains("task not found") {
            err(StatusCode::NOT_FOUND, e)
        } else {
            err(StatusCode::INTERNAL_SERVER_ERROR, e)
        }
    })
}

async fn task_profile_response(
    state: &SharedState,
    task_id: TaskId,
) -> Result<TaskProfileResponse, (StatusCode, Json<ErrorResponse>)> {
    let task_id_for_error = task_id.clone();
    with_task_store_read(state, move |store| {
        store.with_read_snapshot(|store| {
            let task = store
                .get_task(&task_id)?
                .with_context(|| format!("task not found: {}", task_id.0))?;
            if !task.status.is_terminal() {
                bail!(
                    "task profile unavailable for incomplete task {} (status {})",
                    task_id.0,
                    task.status.as_str()
                );
            }
            if !matches!(task.kind, TaskKind::Ask | TaskKind::Retrieve) {
                bail!(
                    "task profile unsupported for {} task: {}",
                    task.kind.as_str(),
                    task_id.0
                );
            }
            let profile = store.get_task_profile(&task_id)?.with_context(|| {
                format!(
                    "task profile unavailable for legacy/no-profile task: {}",
                    task_id.0
                )
            })?;
            Ok(TaskProfileResponse { profile })
        })
    })
    .await
    .map_err(|e| {
        let message = e.to_string();
        if message.contains("task not found") {
            err(StatusCode::NOT_FOUND, e)
        } else if message.contains("incomplete task") {
            err(StatusCode::CONFLICT, e)
        } else if message.contains("unsupported") {
            err(StatusCode::UNPROCESSABLE_ENTITY, e)
        } else if message.contains("legacy/no-profile") {
            err(StatusCode::NOT_FOUND, e)
        } else if message.contains("deserialize task profile") {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                e.context(format!(
                    "stored task profile JSON is malformed for task: {}",
                    task_id_for_error.0
                )),
            )
        } else {
            err(StatusCode::INTERNAL_SERVER_ERROR, e)
        }
    })
}

#[derive(Debug, Deserialize)]
struct TaskListQuery {
    status: Option<String>,
    limit: Option<usize>,
}

async fn list_tasks_handler(
    State(state): State<SharedState>,
    Query(query): Query<TaskListQuery>,
) -> Result<Json<TaskListResponse>, (StatusCode, Json<ErrorResponse>)> {
    with_task_store_read(&state, move |store| {
        store.with_read_snapshot(|store| {
            let filter = task_list_filter(query.status.as_deref())?;
            let limit = query
                .limit
                .unwrap_or(TASK_LIST_DEFAULT_LIMIT)
                .clamp(1, TASK_LIST_MAX_LIMIT);
            let page = store.list_tasks_page(filter, limit)?;
            let tasks = page
                .tasks
                .into_iter()
                .map(|task| {
                    let task = with_queue_details(store, task)?;
                    let redaction = task_telemetry_redaction(store, &task)?;
                    Ok(public_task_summary(task, &redaction))
                })
                .collect::<Result<Vec<_>>>()?;
            let aggregate = task_list_aggregate(store)?;
            Ok::<_, anyhow::Error>(TaskListResponse {
                tasks,
                total: page.total,
                aggregate: Some(aggregate),
            })
        })
    })
    .await
    .map(Json)
    .map_err(|e| {
        if e.to_string().contains("unsupported task status filter") {
            err(StatusCode::BAD_REQUEST, e)
        } else {
            err(StatusCode::INTERNAL_SERVER_ERROR, e)
        }
    })
}

fn task_list_aggregate(store: &Store) -> Result<TaskListAggregate> {
    let active_sample = store.list_tasks_page(
        TaskListFilter::Active,
        TASK_QUEUE_AGGREGATE_ACTIVE_SAMPLE_LIMIT,
    )?;
    let turnover = store.task_turnover_window(TASK_QUEUE_TURNOVER_EVENT_LIMIT)?;
    let active_metadata = store.active_task_metadata_aggregate(TASK_QUEUE_REASON_BUCKET_LIMIT)?;

    Ok(TaskListAggregate {
        active_total: active_sample.total,
        active_sample_size: active_sample.tasks.len(),
        active_sample_limit: TASK_QUEUE_AGGREGATE_ACTIVE_SAMPLE_LIMIT,
        turnover: TaskQueueTurnover {
            window: TaskQueueTurnoverWindow {
                event_sequence_floor: turnover.event_sequence_floor,
                event_sequence_ceiling: turnover.event_sequence_ceiling,
                event_limit: turnover.event_limit,
            },
            recent_terminalized: turnover.recent_terminalized(),
            recent_succeeded: turnover.recent_succeeded,
            recent_failed: turnover.recent_failed,
            recent_cancelled: turnover.recent_cancelled,
            recent_backfilled: turnover.recent_backfilled,
        },
        embedding_wait: TaskEmbeddingWaitAggregate {
            waiting: active_metadata.embedding_waiting,
            oldest_wait_ms: active_metadata.oldest_embedding_wait_ms,
            reason_buckets: task_reason_buckets(active_metadata.embedding_reason_buckets),
        },
        stale_running: TaskStaleRunningAggregate {
            publish_complete_running: active_metadata.publish_complete_running,
            reason_buckets: task_reason_buckets(active_metadata.stale_reason_buckets),
        },
    })
}

fn task_reason_buckets(
    buckets: Vec<verbatim_core::store::TaskReasonCount>,
) -> Vec<TaskReasonBucket> {
    buckets
        .into_iter()
        .map(|bucket| TaskReasonBucket {
            reason: bucket.reason,
            count: bucket.count,
        })
        .collect()
}

fn task_list_filter(status: Option<&str>) -> Result<TaskListFilter> {
    match status.unwrap_or("active") {
        "active" => Ok(TaskListFilter::Active),
        "all" => Ok(TaskListFilter::All),
        other => {
            bail!("unsupported task status filter: {other}")
        }
    }
}

async fn task_events_response(
    state: &SharedState,
    task_id: TaskId,
    after: Option<i64>,
    limit: Option<usize>,
) -> Result<TaskEventsResponse, (StatusCode, Json<ErrorResponse>)> {
    with_task_store_read(state, move |store| {
        let task = store
            .get_task(&task_id)?
            .with_context(|| format!("task not found: {}", task_id.0))?;
        let redaction = task_telemetry_redaction(store, &task)?;
        let events =
            store.list_task_events(&task_id, after, limit.unwrap_or(TASK_WAIT_EVENT_LIMIT))?;
        let events = events
            .into_iter()
            .map(|event| public_task_event_for_source(event, &redaction))
            .collect();
        Ok(TaskEventsResponse { events })
    })
    .await
    .map_err(|e| {
        if e.to_string().contains("task not found") {
            err(StatusCode::NOT_FOUND, e)
        } else {
            err(StatusCode::INTERNAL_SERVER_ERROR, e)
        }
    })
}

async fn ingest_all(
    State(state): State<SharedState>,
    query: Query<IngestQuery>,
) -> Result<Json<IngestResponse>, (StatusCode, Json<ErrorResponse>)> {
    if query.vectors_only && query.force {
        return Err(err(
            StatusCode::BAD_REQUEST,
            anyhow::anyhow!("force is not supported for vectors-only embedding profile builds"),
        ));
    }
    let task_id = create_persisted_task(
        &state,
        TaskKind::Ingest,
        ingest_task_request_metadata_with_queue_claim(
            None,
            query.force,
            query.embedding_profile_id.as_deref(),
            query.vectors_only,
            false,
        ),
    )
    .await?;
    execute_ingest_task(
        state,
        task_id,
        IndexingTaskControls {
            source_id: None,
            force: query.force,
            embedding_profile_id: query.embedding_profile_id.clone(),
            vectors_only: query.vectors_only,
            ingest_batch_id: None,
        },
    )
    .await
    .map(Json)
}

async fn ingest_one(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    query: Query<IngestQuery>,
) -> Result<Json<IngestResponse>, (StatusCode, Json<ErrorResponse>)> {
    if query.force {
        return Err(err(
            StatusCode::BAD_REQUEST,
            anyhow::anyhow!("force is only supported for all-source ingest"),
        ));
    }
    let source_hash = source_hash_for_ingest_task_metadata(&state, &id).await?;
    let request = ingest_task_request_metadata_with_source_hash(
        ingest_task_request_metadata_with_queue_claim(
            Some(&id),
            false,
            query.embedding_profile_id.as_deref(),
            query.vectors_only,
            false,
        ),
        source_hash.as_deref(),
    );
    let task_id = create_persisted_task(&state, TaskKind::Ingest, request).await?;
    execute_ingest_task(
        state,
        task_id,
        IndexingTaskControls {
            source_id: Some(id),
            force: false,
            embedding_profile_id: query.embedding_profile_id.clone(),
            vectors_only: query.vectors_only,
            ingest_batch_id: None,
        },
    )
    .await
    .map(Json)
}

async fn reindex(
    State(state): State<SharedState>,
    Json(req): Json<ReindexRequest>,
) -> Result<Json<ReindexResponse>, (StatusCode, Json<ErrorResponse>)> {
    let controls = resolve_reindex_controls(req).map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let task_id = create_persisted_task(
        &state,
        TaskKind::Ingest,
        reindex_task_request_metadata_with_queue_claim(
            controls.source_id.as_deref(),
            controls.force,
            controls.embedding_profile_id.as_deref(),
            controls.vectors_only,
            false,
        ),
    )
    .await?;
    execute_reindex_task(state, task_id, controls)
        .await
        .map(Json)
}

async fn ask(
    State(state): State<SharedState>,
    Json(req): Json<AskRequest>,
) -> Result<Json<AskResponse>, (StatusCode, Json<ErrorResponse>)> {
    retrieval_readiness_gate(&state)?;
    validate_ask_retrieve_controls(&req)?;
    let task_id = create_persisted_task(
        &state,
        TaskKind::Ask,
        ask_request_metadata(
            &req.question,
            req.source_id.as_deref(),
            req.embedding_profile_id.as_deref(),
            req.show_retrieval,
            req.context_only,
        ),
    )
    .await?;
    execute_ask_task(state, task_id, req).await.map(Json)
}

async fn ask_stream(
    State(state): State<SharedState>,
    Json(req): Json<AskRequest>,
) -> Result<Sse<impl Stream<Item = Result<Event, Infallible>>>, (StatusCode, Json<ErrorResponse>)> {
    retrieval_readiness_gate(&state)?;
    let (tx, rx) = mpsc::channel::<Event>(ASK_STREAM_EVENT_BUFFER);
    let sse_guard = state.idle_reclaim.start_sse().await;
    let idle_exit_sse_guard = state.idle_exit.start_sse();
    if let Err((status, Json(error))) = validate_ask_retrieve_controls(&req) {
        let _ = tx.try_send(sse_error_event(status, error.error));
    } else {
        match create_persisted_task(
            &state,
            TaskKind::Ask,
            ask_request_metadata(
                &req.question,
                req.source_id.as_deref(),
                req.embedding_profile_id.as_deref(),
                req.show_retrieval,
                req.context_only,
            ),
        )
        .await
        {
            Ok(task_id) => {
                tokio::spawn(async move {
                    if let Err((status, Json(error))) =
                        execute_ask_stream_task(state, task_id, req, tx.clone()).await
                    {
                        let _ = tx.send(sse_error_event(status, error.error)).await;
                    }
                });
            }
            Err((status, Json(error))) => {
                let _ = tx.try_send(sse_error_event(status, error.error));
            }
        }
    }

    Ok(Sse::new(stream::unfold(
        (rx, sse_guard, idle_exit_sse_guard),
        |(mut rx, sse_guard, idle_exit_sse_guard): (
            mpsc::Receiver<Event>,
            ActivityGuard,
            IdleExitActivityGuard,
        )| async move {
            rx.recv()
                .await
                .map(|event| (Ok(event), (rx, sse_guard, idle_exit_sse_guard)))
        },
    )))
}

async fn submit_ask_task(
    State(state): State<SharedState>,
    Json(req): Json<AskRequest>,
) -> Result<Json<TaskCreatedResponse>, (StatusCode, Json<ErrorResponse>)> {
    retrieval_readiness_gate(&state)?;
    validate_ask_retrieve_controls(&req)?;
    let task_id = create_persisted_task(
        &state,
        TaskKind::Ask,
        ask_request_metadata(
            &req.question,
            req.source_id.as_deref(),
            req.embedding_profile_id.as_deref(),
            req.show_retrieval,
            req.context_only,
        ),
    )
    .await?;
    spawn_ask_task(state, task_id.clone(), req);
    Ok(Json(TaskCreatedResponse { task_id: task_id.0 }))
}

async fn retrieve(
    State(state): State<SharedState>,
    Json(req): Json<RetrieveRequest>,
) -> Result<Json<RetrieveResponse>, (StatusCode, Json<ErrorResponse>)> {
    retrieval_readiness_gate(&state)?;
    let config = runtime_config_snapshot(&state)
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?
        .config;
    let controls =
        resolve_retrieve_controls(&req, &config).map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let task_id = create_persisted_task(
        &state,
        TaskKind::Retrieve,
        retrieve_request_metadata(
            &req.question,
            req.source_id.as_deref(),
            req.embedding_profile_id.as_deref(),
            controls.limit,
            controls.page_size,
            controls.page,
        ),
    )
    .await?;
    execute_retrieve_task(state, task_id, req, controls)
        .await
        .map(Json)
}

async fn submit_ingest_task(
    State(state): State<SharedState>,
    Json(req): Json<TaskIngestRequest>,
) -> Result<Json<TaskCreatedResponse>, (StatusCode, Json<ErrorResponse>)> {
    retrieval_readiness_gate(&state)?;
    if req.source_id.is_some() && req.force {
        return Err(err(
            StatusCode::BAD_REQUEST,
            anyhow::anyhow!("force is only supported for all-source ingest"),
        ));
    }
    if req.vectors_only && req.force {
        return Err(err(
            StatusCode::BAD_REQUEST,
            anyhow::anyhow!("force is not supported for vectors-only embedding profile builds"),
        ));
    }
    if req.source_id.is_none() {
        let task_id = create_background_ingest_batch(&state, req).await?;
        schedule_ingest_queue(Arc::clone(&state));
        return Ok(Json(TaskCreatedResponse { task_id: task_id.0 }));
    }
    let Some(source_id) = req.source_id.clone() else {
        return Err(err(
            StatusCode::BAD_REQUEST,
            anyhow::anyhow!("source_id is required for source-specific ingest"),
        ));
    };
    let source_hash = source_hash_for_ingest_task_metadata(&state, &source_id).await?;
    let task_id = TaskId::new();
    let request = ingest_task_request_metadata_with_source_hash(
        ingest_task_request_metadata_with_queue_claim_and_batch(
            Some(source_id.as_str()),
            req.force,
            req.embedding_profile_id.as_deref(),
            req.vectors_only,
            true,
            None,
        ),
        source_hash.as_deref(),
    );
    let task_id = create_persisted_task_with_id(&state, task_id, TaskKind::Ingest, request).await?;
    schedule_ingest_queue(Arc::clone(&state));
    Ok(Json(TaskCreatedResponse { task_id: task_id.0 }))
}

async fn submit_reindex_task(
    State(state): State<SharedState>,
    Json(req): Json<ReindexRequest>,
) -> Result<Json<TaskCreatedResponse>, (StatusCode, Json<ErrorResponse>)> {
    retrieval_readiness_gate(&state)?;
    let controls = resolve_reindex_controls(req).map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    validate_requested_source_exists(&state, controls.source_id.as_deref()).await?;
    let task_id = TaskId::new();
    let ingest_batch_id = controls.source_id.is_none().then(|| task_id.0.clone());
    let task_id = create_persisted_task_with_id(
        &state,
        task_id,
        TaskKind::Ingest,
        reindex_task_request_metadata_with_queue_claim_and_batch(
            controls.source_id.as_deref(),
            controls.force,
            controls.embedding_profile_id.as_deref(),
            controls.vectors_only,
            true,
            ingest_batch_id.as_deref(),
        ),
    )
    .await?;
    schedule_ingest_queue(Arc::clone(&state));
    Ok(Json(TaskCreatedResponse { task_id: task_id.0 }))
}

async fn show_task(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<TaskSummaryResponse>, (StatusCode, Json<ErrorResponse>)> {
    task_summary_response(&state, TaskId(id)).await.map(Json)
}

async fn task_profile_handler(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<TaskProfileResponse>, (StatusCode, Json<ErrorResponse>)> {
    task_profile_response(&state, TaskId(id)).await.map(Json)
}

async fn list_task_events_handler(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Query(query): Query<TaskEventsQuery>,
) -> Result<Json<TaskEventsResponse>, (StatusCode, Json<ErrorResponse>)> {
    task_events_response(&state, TaskId(id), query.after, query.limit)
        .await
        .map(Json)
}

async fn cancel_task_handler(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<TaskSummaryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let task_id = TaskId(id);
    let changed = cancel_task_record(&state, &task_id).await?;
    if changed {
        schedule_ingest_queue(Arc::clone(&state));
    }
    if !changed {
        let response = task_summary_response(&state, task_id.clone()).await?;
        if response.task.status.is_terminal() {
            return Ok(Json(response));
        }
    }
    task_summary_response(&state, task_id).await.map(Json)
}

async fn resume_task_handler(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<TaskSummaryResponse>, (StatusCode, Json<ErrorResponse>)> {
    let task_id = TaskId(id);
    let plan = resume_task_record(&state, &task_id).await?;
    if plan.queue_claimable {
        schedule_ingest_queue(Arc::clone(&state));
    } else {
        spawn_resumed_indexing_task(Arc::clone(&state), plan);
    }
    task_summary_response(&state, task_id).await.map(Json)
}

async fn resume_task_record(
    state: &SharedState,
    task_id: &TaskId,
) -> Result<ResumeTaskPlan, (StatusCode, Json<ErrorResponse>)> {
    let task_id = task_id.clone();
    with_task_store_write(state, move |store| {
        let task = store
            .get_task(&task_id)?
            .with_context(|| format!("task not found: {}", task_id.0))?;
        if task.status != TaskStatus::Failed {
            bail!(
                "task is not resumable from status {}; only failed ingest/reindex tasks can be resumed",
                task.status.as_str()
            );
        }
        let Some(plan) = resumable_task_plan(&task)? else {
            bail!(
                "task is not resumable; ask/retrieve task metadata intentionally omits raw question text, and this task has no executable ingest/reindex request"
            );
        };
        let previous_error = task.error.as_deref();
        if !store.resume_failed_task(&task_id)? {
            bail!("task changed before it could be resumed: {}", task_id.0);
        }
        let payload = resumability_payload(
            &plan,
            previous_error,
            "task resumed and requeued by task id",
        );
        store.insert_task_event(&task_id, "resumed", "task resumed", &payload)?;
        Ok(plan)
    })
    .await
    .map_err(|e| {
        let message = e.to_string();
        if message.contains("task not found") {
            err(StatusCode::NOT_FOUND, e)
        } else if message.contains("not resumable") || message.contains("changed before") {
            err(StatusCode::CONFLICT, e)
        } else {
            err(StatusCode::INTERNAL_SERVER_ERROR, e)
        }
    })
}

fn spawn_resumed_indexing_task(state: SharedState, plan: ResumeTaskPlan) {
    tokio::spawn(async move {
        let task_id = plan.task_id.clone();
        let result = match plan.operation {
            ResumableIngestOperation::Ingest => {
                execute_ingest_task(Arc::clone(&state), task_id.clone(), plan.controls)
                    .await
                    .map(|_| ())
            }
            ResumableIngestOperation::Reindex => {
                execute_reindex_task(Arc::clone(&state), task_id.clone(), plan.controls)
                    .await
                    .map(|_| ())
            }
        };
        if let Err((status, Json(error))) = result {
            tracing::warn!(
                task_id = %task_id.0,
                status = %status,
                error = %error.error,
                "resumed task execution failed"
            );
        }
    });
}

async fn wait_task(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Query(query): Query<TaskEventsQuery>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = mpsc::channel::<Event>(TASK_WAIT_EVENT_BUFFER);
    let sse_guard = state.idle_reclaim.start_sse().await;
    let idle_exit_sse_guard = state.idle_exit.start_sse();
    tokio::spawn(async move {
        let task_id = TaskId(id);
        let mut after = query.after;
        let limit = query.limit.unwrap_or(TASK_WAIT_EVENT_LIMIT);
        loop {
            match task_wait_snapshot(&state, task_id.clone(), after, limit).await {
                Ok(wait_event) => {
                    after = wait_event
                        .events
                        .last()
                        .map(|event| event.sequence)
                        .or(after);
                    let terminal = wait_event.terminal;
                    if tx.send(sse_json_event("task", &wait_event)).await.is_err() {
                        break;
                    }
                    if terminal {
                        break;
                    }
                }
                Err((status, Json(error))) => {
                    let _ = tx.send(sse_error_event(status, error.error)).await;
                    break;
                }
            }
            tokio::time::sleep(TASK_WAIT_POLL_INTERVAL).await;
        }
    });

    Sse::new(stream::unfold(
        (rx, sse_guard, idle_exit_sse_guard),
        |(mut rx, sse_guard, idle_exit_sse_guard): (
            mpsc::Receiver<Event>,
            ActivityGuard,
            IdleExitActivityGuard,
        )| async move {
            rx.recv()
                .await
                .map(|event| (Ok(event), (rx, sse_guard, idle_exit_sse_guard)))
        },
    ))
}

async fn execute_ask_task(
    state: SharedState,
    task_id: TaskId,
    req: AskRequest,
) -> Result<AskResponse, (StatusCode, Json<ErrorResponse>)> {
    let result = execute_ask_task_inner(Arc::clone(&state), &task_id, req).await;
    if let Err((_, Json(error))) = &result {
        let _ = finish_task_failed_from_response(&state, &task_id, error).await;
    }
    result
}

async fn execute_retrieve_task(
    state: SharedState,
    task_id: TaskId,
    req: RetrieveRequest,
    controls: EffectiveRetrieveControls,
) -> Result<RetrieveResponse, (StatusCode, Json<ErrorResponse>)> {
    let result = execute_retrieve_task_inner(Arc::clone(&state), &task_id, req, controls).await;
    if let Err((_, Json(error))) = &result {
        let _ = finish_task_failed_from_response(&state, &task_id, error).await;
    }
    result
}

async fn execute_retrieve_task_inner(
    state: SharedState,
    task_id: &TaskId,
    req: RetrieveRequest,
    controls: EffectiveRetrieveControls,
) -> Result<RetrieveResponse, (StatusCode, Json<ErrorResponse>)> {
    let execution_started = Instant::now();
    ensure_task_started(&state, task_id).await?;
    let queue_wait_ms = task_queue_wait_ms(&state, task_id).await?;
    let question = req.question;
    let source_id = req.source_id.map(SourceId);
    let collection_filter = req.collection_filter;
    let embedding_profile_id = parse_embedding_profile_id(
        req.embedding_profile_id.as_deref(),
        &controls.config.embedding.profile_id,
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let freshness_profile_id = controls
        .config
        .embedding
        .enabled
        .then(|| embedding_profile_id.clone());
    refresh_query_embedding_profile_for_collection_filter(
        &state,
        &collection_filter,
        controls.config.embedding.enabled,
        &embedding_profile_id,
    )
    .await?;
    let query_scope =
        resolve_query_scope(&state, source_id, collection_filter, freshness_profile_id).await?;

    let timing = PhaseTiming::start("retrieval");
    record_task_progress(
        &state,
        task_id,
        timing
            .progress_snapshot()
            .with_counter("retrieval_candidates", 0, None)
            .with_recent_status("retrieving evidence")
            .with_active_worker_kind(TaskKind::Retrieve.as_str()),
    )
    .await;
    let mut retrieved_context = prepare_retrieve_context(
        Arc::clone(&state),
        &question,
        query_scope.source_filter.clone(),
        &embedding_profile_id,
        &controls,
    )
    .await?;
    filter_generated_retrieval_evidence(
        &mut retrieved_context.results,
        &mut retrieved_context.debug,
    );
    let RetrievedContext {
        results,
        debug,
        source_paths,
    } = retrieved_context;
    let mut retrieval_progress = timing
        .progress_snapshot()
        .with_counter(
            "dense_candidates",
            debug
                .candidate_counters
                .returned_k(SpanKind::DenseRetrieval),
            None,
        )
        .with_counter(
            "bm25_candidates",
            debug
                .candidate_counters
                .returned_k(SpanKind::LexicalRetrieval),
            None,
        )
        .with_counter(
            "retrieval_candidates",
            debug.candidate_counters.fused(),
            None,
        )
        .with_counter("evidence", debug.final_evidence_count as u64, None)
        .with_recent_status(format!(
            "retrieved {} evidence entries",
            debug.final_evidence_count
        ))
        .with_active_worker_kind(TaskKind::Retrieve.as_str());
    if let Some(latency_ms) = debug.query_embedding_latency_ms {
        retrieval_progress.set_endpoint(TaskEndpointSummary::single_call("embedding", latency_ms));
    }
    if let Some(top_n) = debug.reranker.top_n {
        retrieval_progress.set_counter(
            "rerank_top_n",
            top_n as u64,
            debug.reranker.candidate_count.map(|count| count as u64),
        );
    }
    if let Some(latency_ms) = debug.reranker.latency_ms {
        let endpoint = match debug.reranker.status {
            verbatim_core::types::RetrievalRerankStatus::Fallback => {
                TaskEndpointSummary::failed_call(
                    "reranker",
                    Some(latency_ms),
                    debug
                        .reranker
                        .reason
                        .clone()
                        .unwrap_or_else(|| "rerank fallback".into()),
                )
            }
            verbatim_core::types::RetrievalRerankStatus::Succeeded => {
                TaskEndpointSummary::single_call("reranker", latency_ms)
            }
            _ => TaskEndpointSummary::single_call("reranker", latency_ms),
        };
        retrieval_progress.set_endpoint(endpoint);
    }
    let retrieval_response_total = if controls.passage {
        retrieve_passage_group_count(&results)
    } else {
        debug.display_evidence_count
    };
    let retrieval_timing = timing.finish(retrieval_span_metadata(
        serde_json::json!({
            "result_count": results.len(),
            "returned_results": page_len(
                retrieval_response_total,
                controls.limit,
                controls.page_size,
                controls.page,
            ),
            "rerank_enabled": controls.rerank_config.enabled,
            "dense_top_k": controls.retrieval_config.dense_top_k,
            "bm25_top_k": controls.retrieval_config.bm25_top_k,
            "dense_vector_path": debug.dense_vector_path,
        }),
        Some(&debug),
    ));
    record_task_progress(&state, task_id, retrieval_progress).await;
    let retrieval_started_at = retrieval_timing.started_at.clone();
    record_task_span(&state, task_id, retrieval_timing.clone()).await?;
    record_task_event(
        &state,
        task_id,
        "phase",
        "context retrieval complete",
        serde_json::json!({
            "result_count": results.len(),
            "rerank_enabled": controls.rerank_config.enabled,
            "dense_vector_path": debug.dense_vector_path,
        }),
    )
    .await?;

    let configured_rerank_top_n = controls.rerank_config.top_n;
    let mut profile_debug = debug.clone();
    let profile_controls = retrieve_profile_controls(
        &controls,
        &embedding_profile_id,
        &query_scope,
        &profile_debug,
    );
    let mut task_local_spans = debug.local_spans_ms.clone();
    let response_started = Instant::now();
    let response_input = RetrieveResponseInput {
        task_id: task_id.clone(),
        query: question,
        source_filter: query_scope.source_id,
        collection_filter: query_scope.collection_filter,
        collection_provenance: query_scope.collection_provenance,
        embedding_profile_id,
        controls,
        results,
        debug,
        source_paths,
        retrieval_ms: retrieval_timing.duration_ms,
    };
    let mut response = with_task_store_read(&state, move |store| {
        retrieve_response(store, response_input)
    })
    .await
    .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    let response_formatting_ms = elapsed_ms(response_started);
    task_local_spans.response_formatting_ms = response_formatting_ms;
    profile_debug.local_spans_ms.response_formatting_ms = response_formatting_ms;
    if let Some(response_debug) = response.debug.as_mut() {
        response_debug.local_spans_ms.response_formatting_ms = response_formatting_ms;
    }
    record_retrieve_local_spans(&state, task_id, &retrieval_started_at, &task_local_spans).await?;

    let profile = TaskProfile {
        schema_version: verbatim_core::task::TASK_PROFILE_SCHEMA_VERSION,
        task_id: task_id.clone(),
        task_kind: TaskKind::Retrieve,
        status: TaskStatus::Succeeded,
        queue_wait_ms,
        total_wall_ms: queue_wait_ms.saturating_add(elapsed_ms(execution_started)),
        controls: profile_controls,
        resources: task_resource_profile_from_state(&state),
        endpoints: retrieve_endpoint_summaries(&profile_debug),
        retrieve: Some(retrieve_task_profile_from_debug(
            &profile_debug,
            configured_rerank_top_n,
            response.total_results,
            response.returned_results,
        )),
        ask: None,
    };
    finish_task_success_with_profile(
        &state,
        task_id,
        retrieve_result_metadata(
            response.total_results,
            response.returned_results,
            response.controls.rerank_enabled,
        ),
        profile,
    )
    .await?;
    Ok(response)
}

async fn execute_ask_task_inner(
    state: SharedState,
    task_id: &TaskId,
    req: AskRequest,
) -> Result<AskResponse, (StatusCode, Json<ErrorResponse>)> {
    if req.context_only {
        return execute_context_only_ask_task_inner(state, task_id, req).await;
    }

    let config = runtime_config_snapshot(&state)
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?
        .config;
    execute_ask_task_inner_with_config(state, task_id, req, config).await
}

async fn execute_ask_task_inner_with_config(
    state: SharedState,
    task_id: &TaskId,
    req: AskRequest,
    config: Config,
) -> Result<AskResponse, (StatusCode, Json<ErrorResponse>)> {
    let execution_started = Instant::now();
    ensure_task_started(&state, task_id).await?;
    let queue_wait_ms = task_queue_wait_ms(&state, task_id).await?;
    let question = req.question;
    let source_id = req.source_id.map(SourceId);
    let collection_filter = req.collection_filter;
    let embedding_profile_id = parse_embedding_profile_id(
        req.embedding_profile_id.as_deref(),
        &config.embedding.profile_id,
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let show_retrieval = req.show_retrieval;
    let freshness_profile_id = config
        .embedding
        .enabled
        .then(|| embedding_profile_id.clone());
    refresh_query_embedding_profile_for_collection_filter(
        &state,
        &collection_filter,
        config.embedding.enabled,
        &embedding_profile_id,
    )
    .await?;
    let query_scope =
        resolve_query_scope(&state, source_id, collection_filter, freshness_profile_id).await?;

    let timing = PhaseTiming::start("retrieval");
    record_task_progress(
        &state,
        task_id,
        timing
            .progress_snapshot()
            .with_counter("retrieval_candidates", 0, None)
            .with_recent_status("retrieving evidence")
            .with_active_worker_kind(TaskKind::Ask.as_str()),
    )
    .await;
    let (results, generation_context, retrieval_debug) = prepare_generation_context(
        Arc::clone(&state),
        &question,
        query_scope.source_filter.clone(),
        &embedding_profile_id,
        &config,
        show_retrieval,
    )
    .await?;
    let mut retrieval_progress = timing
        .progress_snapshot()
        .with_counter("evidence", results.len() as u64, None)
        .with_recent_status(format!("retrieved {} result(s)", results.len()))
        .with_active_worker_kind(TaskKind::Ask.as_str());
    if let Some(debug) = &retrieval_debug {
        retrieval_progress.set_counter(
            "dense_candidates",
            debug
                .candidate_counters
                .returned_k(SpanKind::DenseRetrieval),
            None,
        );
        retrieval_progress.set_counter(
            "bm25_candidates",
            debug
                .candidate_counters
                .returned_k(SpanKind::LexicalRetrieval),
            None,
        );
        retrieval_progress.set_counter(
            "retrieval_candidates",
            debug.candidate_counters.fused(),
            None,
        );
        if let Some(latency_ms) = debug.query_embedding_latency_ms {
            retrieval_progress
                .set_endpoint(TaskEndpointSummary::single_call("embedding", latency_ms));
        }
        if let Some(latency_ms) = debug.reranker.latency_ms {
            retrieval_progress
                .set_endpoint(TaskEndpointSummary::single_call("reranker", latency_ms));
        }
    }
    record_task_progress(&state, task_id, retrieval_progress).await;
    record_task_span(
        &state,
        task_id,
        timing.finish(retrieval_span_metadata(
            serde_json::json!({
                "result_count": results.len(),
                "retrieval_debug": retrieval_debug.is_some(),
                "dense_vector_path": retrieval_debug.as_ref().map(|debug| debug.dense_vector_path),
            }),
            retrieval_debug.as_ref(),
        )),
    )
    .await?;
    record_task_event(
        &state,
        task_id,
        "phase",
        "retrieval complete",
        serde_json::json!({
            "result_count": results.len(),
            "dense_vector_path": retrieval_debug.as_ref().map(|debug| debug.dense_vector_path),
        }),
    )
    .await?;

    let timing = PhaseTiming::start("chat");
    record_task_progress(
        &state,
        task_id,
        timing
            .progress_snapshot()
            .with_recent_status("waiting for chat model")
            .with_active_worker_kind(TaskKind::Ask.as_str()),
    )
    .await;
    let chat_started = Instant::now();
    let generator = Generator::new(&config.chat, &config.verifier);
    let gen_result = match generator
        .generate_with_context(&question, &results, &generation_context)
        .await
    {
        Ok(result) => result,
        Err(error) => {
            record_task_progress(
                &state,
                task_id,
                timing
                    .progress_snapshot()
                    .with_endpoint(TaskEndpointSummary::failed_call(
                        "chat",
                        Some(elapsed_ms(chat_started)),
                        error.to_string(),
                    ))
                    .with_recent_status("chat model failed")
                    .with_active_worker_kind(TaskKind::Ask.as_str()),
            )
            .await;
            return Err(err(StatusCode::INTERNAL_SERVER_ERROR, error));
        }
    };
    let chat_timing = timing.finish(serde_json::json!({
        "citation_count": gen_result.citations.len(),
        "verified": gen_result.verified,
    }));
    record_task_progress(
        &state,
        task_id,
        TaskProgressSnapshot {
            phase: Some(verbatim_core::task::TaskProgressPhase {
                name: "chat".into(),
                started_at: chat_timing.started_at.clone(),
                elapsed_ms: chat_timing.duration_ms,
            }),
            ..TaskProgressSnapshot::default()
        }
        .with_endpoint(TaskEndpointSummary::single_call(
            "chat",
            chat_timing.duration_ms,
        ))
        .with_counter("citations", gen_result.citations.len() as u64, None)
        .with_recent_status("chat complete")
        .with_active_worker_kind(TaskKind::Ask.as_str()),
    )
    .await;
    record_task_span(&state, task_id, chat_timing.clone()).await?;
    record_task_span(
        &state,
        task_id,
        verbatim_core::task::FinishedPhaseTiming {
            phase: "model_call".into(),
            ..chat_timing
        },
    )
    .await?;

    let generation_telemetry = gen_result.telemetry.clone();
    let retrieval_for_response = if show_retrieval {
        retrieval_debug.clone()
    } else {
        None
    };
    let profile_controls = ask_profile_controls(
        &config,
        &embedding_profile_id,
        &query_scope,
        retrieval_debug.as_ref(),
        show_retrieval,
    );
    let response_started = Instant::now();
    let response = AskResponse {
        answer: gen_result.answer.clone(),
        generated_interpretation: Some(GeneratedInterpretationResponse {
            text: gen_result.answer,
        }),
        citations: gen_result
            .citations
            .into_iter()
            .map(|citation| {
                citation_response_with_collections(citation, &query_scope.collection_provenance)
            })
            .collect(),
        verified: gen_result.verified,
        retrieval: retrieval_for_response,
        context: None,
        collection_filter: query_scope.collection_filter,
    };
    let response_formatting_ms = elapsed_ms(response_started);
    let mut endpoints = retrieval_debug
        .as_ref()
        .map(retrieve_endpoint_summaries)
        .unwrap_or_default();
    endpoints.extend(ask_endpoint_summaries(&generation_telemetry));
    let retrieve_profile = retrieval_debug.as_ref().map(|debug| {
        retrieve_task_profile_from_debug(debug, config.rerank.top_n, results.len(), results.len())
    });
    let profile = TaskProfile {
        schema_version: verbatim_core::task::TASK_PROFILE_SCHEMA_VERSION,
        task_id: task_id.clone(),
        task_kind: TaskKind::Ask,
        status: TaskStatus::Succeeded,
        queue_wait_ms,
        total_wall_ms: queue_wait_ms.saturating_add(elapsed_ms(execution_started)),
        controls: profile_controls,
        resources: task_resource_profile_from_state(&state),
        endpoints,
        retrieve: retrieve_profile,
        ask: Some(ask_task_profile_from_telemetry(
            &generation_telemetry,
            response_formatting_ms,
            &response.answer,
            response.citations.len(),
            response.retrieval.is_some(),
        )),
    };
    finish_task_success_with_profile(
        &state,
        task_id,
        ask_result_metadata(
            &response.answer,
            response.citations.len(),
            response.verified,
            response.retrieval.is_some(),
        ),
        profile,
    )
    .await?;
    Ok(response)
}

async fn execute_ask_stream_task(
    state: SharedState,
    task_id: TaskId,
    req: AskRequest,
    tx: mpsc::Sender<Event>,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let result = execute_ask_stream_task_inner(Arc::clone(&state), &task_id, req, tx).await;
    if let Err((_, Json(error))) = &result {
        let _ = finish_task_failed_from_response(&state, &task_id, error).await;
    }
    result
}

async fn execute_ask_stream_task_inner(
    state: SharedState,
    task_id: &TaskId,
    req: AskRequest,
    tx: mpsc::Sender<Event>,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if req.context_only {
        let response =
            execute_context_only_ask_task_inner(Arc::clone(&state), task_id, req).await?;
        send_stream_event(&tx, sse_json_event("answer", &response)).await?;
        return Ok(());
    }

    let config = runtime_config_snapshot(&state)
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?
        .config;
    if config.verifier.enabled {
        let response =
            execute_ask_task_inner_with_config(Arc::clone(&state), task_id, req, config).await?;
        send_stream_event(&tx, sse_json_event("answer", &response)).await?;
        return Ok(());
    }

    ensure_task_started(&state, task_id).await?;
    let question = req.question;
    let source_id = req.source_id.map(SourceId);
    let collection_filter = req.collection_filter;
    let embedding_profile_id = parse_embedding_profile_id(
        req.embedding_profile_id.as_deref(),
        &config.embedding.profile_id,
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let show_retrieval = req.show_retrieval;
    let freshness_profile_id = config
        .embedding
        .enabled
        .then(|| embedding_profile_id.clone());
    refresh_query_embedding_profile_for_collection_filter(
        &state,
        &collection_filter,
        config.embedding.enabled,
        &embedding_profile_id,
    )
    .await?;
    let query_scope =
        resolve_query_scope(&state, source_id, collection_filter, freshness_profile_id).await?;

    let timing = PhaseTiming::start("retrieval");
    record_task_progress(
        &state,
        task_id,
        timing
            .progress_snapshot()
            .with_counter("retrieval_candidates", 0, None)
            .with_recent_status("retrieving evidence")
            .with_active_worker_kind(TaskKind::Ask.as_str()),
    )
    .await;
    let (results, generation_context, retrieval_debug) = prepare_generation_context(
        Arc::clone(&state),
        &question,
        query_scope.source_filter.clone(),
        &embedding_profile_id,
        &config,
        show_retrieval,
    )
    .await?;
    record_task_progress(
        &state,
        task_id,
        timing
            .progress_snapshot()
            .with_counter("evidence", results.len() as u64, None)
            .with_recent_status(format!("retrieved {} result(s)", results.len()))
            .with_active_worker_kind(TaskKind::Ask.as_str()),
    )
    .await;
    record_task_span(
        &state,
        task_id,
        timing.finish(retrieval_span_metadata(
            serde_json::json!({
                "result_count": results.len(),
                "retrieval_debug": retrieval_debug.is_some(),
            }),
            retrieval_debug.as_ref(),
        )),
    )
    .await?;

    let tx_tokens = tx.clone();
    let timing = PhaseTiming::start("chat");
    record_task_progress(
        &state,
        task_id,
        timing
            .progress_snapshot()
            .with_recent_status("waiting for streaming chat model")
            .with_active_worker_kind(TaskKind::Ask.as_str()),
    )
    .await;
    let chat_started = Instant::now();
    let streamed_bytes = Arc::new(AtomicU64::new(0));
    let streamed_chunks = Arc::new(AtomicU64::new(0));
    let first_token_latency_ms = Arc::new(AtomicU64::new(0));
    let streamed_bytes_for_callback = Arc::clone(&streamed_bytes);
    let streamed_chunks_for_callback = Arc::clone(&streamed_chunks);
    let first_token_for_callback = Arc::clone(&first_token_latency_ms);
    let generator = Generator::new(&config.chat, &config.verifier);
    let gen_result = match generator
        .generate_streaming_with_context(&question, &results, &generation_context, move |delta| {
            streamed_bytes_for_callback.fetch_add(delta.len() as u64, Ordering::Relaxed);
            streamed_chunks_for_callback.fetch_add(1, Ordering::Relaxed);
            let _ = first_token_for_callback.compare_exchange(
                0,
                elapsed_ms(chat_started),
                Ordering::Relaxed,
                Ordering::Relaxed,
            );
            try_send_stream_event(
                &tx_tokens,
                sse_json_event(
                    "token",
                    &AskTokenEvent {
                        text: delta.to_string(),
                    },
                ),
            )?;
            Ok(())
        })
        .await
    {
        Ok(result) => result,
        Err(error) => {
            record_task_progress(
                &state,
                task_id,
                timing
                    .progress_snapshot()
                    .with_endpoint(TaskEndpointSummary::failed_call(
                        "chat",
                        Some(elapsed_ms(chat_started)),
                        error.to_string(),
                    ))
                    .with_counter(
                        "chat_bytes_streamed",
                        streamed_bytes.load(Ordering::Relaxed),
                        None,
                    )
                    .with_counter(
                        "chat_chunks_streamed",
                        streamed_chunks.load(Ordering::Relaxed),
                        None,
                    )
                    .with_recent_status("streaming chat model failed")
                    .with_active_worker_kind(TaskKind::Ask.as_str()),
            )
            .await;
            return Err(err(StatusCode::INTERNAL_SERVER_ERROR, error));
        }
    };
    let chat_timing = timing.finish(serde_json::json!({
        "citation_count": gen_result.citations.len(),
        "verified": gen_result.verified,
        "streaming": true,
        "streamed_bytes": streamed_bytes.load(Ordering::Relaxed),
        "streamed_chunks": streamed_chunks.load(Ordering::Relaxed),
        "first_token_latency_ms": first_token_latency_ms.load(Ordering::Relaxed),
    }));
    let first_token_ms = first_token_latency_ms.load(Ordering::Relaxed);
    let endpoint = if first_token_ms > 0 {
        TaskEndpointSummary::single_call("chat", chat_timing.duration_ms)
            .with_first_token_latency_ms(first_token_ms)
    } else {
        TaskEndpointSummary::single_call("chat", chat_timing.duration_ms)
    };
    record_task_progress(
        &state,
        task_id,
        TaskProgressSnapshot {
            phase: Some(verbatim_core::task::TaskProgressPhase {
                name: "chat".into(),
                started_at: chat_timing.started_at.clone(),
                elapsed_ms: chat_timing.duration_ms,
            }),
            ..TaskProgressSnapshot::default()
        }
        .with_endpoint(endpoint)
        .with_counter(
            "chat_bytes_streamed",
            streamed_bytes.load(Ordering::Relaxed),
            None,
        )
        .with_counter(
            "chat_chunks_streamed",
            streamed_chunks.load(Ordering::Relaxed),
            None,
        )
        .with_counter("citations", gen_result.citations.len() as u64, None)
        .with_recent_status("streaming chat complete")
        .with_active_worker_kind(TaskKind::Ask.as_str()),
    )
    .await;
    record_task_span(&state, task_id, chat_timing.clone()).await?;
    record_task_span(
        &state,
        task_id,
        verbatim_core::task::FinishedPhaseTiming {
            phase: "model_call".into(),
            ..chat_timing
        },
    )
    .await?;

    let citation_count = gen_result.citations.len();
    send_stream_event(
        &tx,
        sse_json_event(
            "citation",
            &AskCitationEvent {
                citations: gen_result
                    .citations
                    .iter()
                    .cloned()
                    .map(|citation| {
                        citation_response_with_collections(
                            citation,
                            &query_scope.collection_provenance,
                        )
                    })
                    .collect(),
                verified: gen_result.verified,
            },
        ),
    )
    .await?;

    if show_retrieval {
        if let Some(debug) = retrieval_debug {
            send_stream_event(&tx, sse_json_event("retrieval", &debug)).await?;
        }
    }
    if let Some(collection_filter) = &query_scope.collection_filter {
        send_stream_event(&tx, sse_json_event("collection_filter", collection_filter)).await?;
    }

    finish_task_success(
        &state,
        task_id,
        ask_result_metadata(
            &gen_result.answer,
            citation_count,
            gen_result.verified,
            show_retrieval,
        ),
    )
    .await?;
    Ok(())
}

async fn execute_context_only_ask_task_inner(
    state: SharedState,
    task_id: &TaskId,
    req: AskRequest,
) -> Result<AskResponse, (StatusCode, Json<ErrorResponse>)> {
    let retrieve_req = context_only_retrieve_request(req);
    let config = runtime_config_snapshot(&state)
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?
        .config;
    let controls = resolve_retrieve_controls(&retrieve_req, &config)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let context = execute_retrieve_task_inner(state, task_id, retrieve_req, controls).await?;
    let collection_filter = context.collection_filter.clone();
    Ok(AskResponse {
        answer: String::new(),
        generated_interpretation: None,
        citations: Vec::new(),
        verified: false,
        retrieval: None,
        context: Some(context),
        collection_filter,
    })
}

fn context_only_retrieve_request(req: AskRequest) -> RetrieveRequest {
    RetrieveRequest {
        question: req.question,
        source_id: req.source_id,
        collection_filter: req.collection_filter,
        embedding_profile_id: req.embedding_profile_id,
        limit: req.limit,
        page_size: req.page_size,
        page: req.page,
        fast: false,
        rerank: None,
        dense_top_k: None,
        bm25_top_k: None,
        rerank_top_n: None,
        bypass_cache: false,
        include_debug: req.show_retrieval,
        include_debug_packs: false,
        include_locator: true,
        passage: false,
    }
}

fn validate_ask_retrieve_controls(
    req: &AskRequest,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if req.context_only || (req.limit.is_none() && req.page_size.is_none() && req.page.is_none()) {
        return Ok(());
    }

    Err(err(
        StatusCode::BAD_REQUEST,
        anyhow::anyhow!("limit, page_size, and page are only supported when context_only is true"),
    ))
}

fn spawn_ask_task(state: SharedState, task_id: TaskId, req: AskRequest) {
    tokio::spawn(async move {
        let _ = execute_ask_task(state, task_id, req).await;
    });
}

#[derive(Debug, Clone)]
struct EffectiveRetrieveControls {
    limit: usize,
    page_size: usize,
    page: usize,
    include_debug: bool,
    include_debug_packs: bool,
    include_locator: bool,
    passage: bool,
    bypass_cache: bool,
    fast: bool,
    config: Config,
    retrieval_config: RetrievalConfig,
    rerank_config: RerankConfig,
}

struct RetrievedContext {
    results: Vec<RetrievalResult>,
    debug: RetrievalDebug,
    source_paths: HashMap<String, String>,
}

fn retrieve_debug_display_scope(controls: &EffectiveRetrieveControls) -> RetrievalDisplayScope {
    RetrievalDisplayScope::page(controls.limit, controls.page_size, controls.page)
}

fn empty_retrieval_display_scope() -> RetrievalDisplayScope {
    RetrievalDisplayScope::Window { start: 0, len: 0 }
}

fn retrieve_debug_options(controls: &EffectiveRetrieveControls) -> RetrievalDebugOptions {
    if controls.passage {
        let empty_scope = empty_retrieval_display_scope();
        let canonical_budget = RetrievalCanonicalSelectionBudget::new(empty_scope, empty_scope);
        return if controls.include_debug && controls.include_debug_packs {
            RetrievalDebugOptions::full(canonical_budget)
        } else {
            RetrievalDebugOptions::compact(canonical_budget)
        };
    }

    let canonical_budget =
        RetrievalCanonicalSelectionBudget::scoped(retrieve_debug_display_scope(controls));
    if controls.include_debug && controls.include_debug_packs {
        RetrievalDebugOptions::full(canonical_budget)
    } else {
        RetrievalDebugOptions::compact(canonical_budget)
    }
}

#[derive(Debug, Clone)]
struct QueryScope {
    source_id: Option<SourceId>,
    source_filter: Option<HashSet<SourceId>>,
    collection_filter: Option<CollectionFilterResponse>,
    collection_provenance: HashMap<String, Vec<CollectionResultProvenance>>,
}

struct RetrieveResponseInput {
    task_id: TaskId,
    query: String,
    source_filter: Option<SourceId>,
    collection_filter: Option<CollectionFilterResponse>,
    collection_provenance: HashMap<String, Vec<CollectionResultProvenance>>,
    embedding_profile_id: EmbeddingProfileId,
    controls: EffectiveRetrieveControls,
    results: Vec<RetrievalResult>,
    debug: RetrievalDebug,
    source_paths: HashMap<String, String>,
    retrieval_ms: u64,
}

fn resolve_retrieve_controls(
    req: &RetrieveRequest,
    config: &Config,
) -> Result<EffectiveRetrieveControls> {
    let mut retrieval_config = config.retrieval.clone();
    let mut rerank_config = config.rerank.clone();

    if req.fast {
        retrieval_config.dense_top_k = FAST_RETRIEVAL_TOP_K;
        retrieval_config.bm25_top_k = FAST_RETRIEVAL_TOP_K;
        rerank_config.enabled = false;
        rerank_config.top_n = 0;
    }

    if let Some(dense_top_k) = req.dense_top_k {
        retrieval_config.dense_top_k = nonzero_control("dense_top_k", dense_top_k)?;
    }
    if let Some(bm25_top_k) = req.bm25_top_k {
        retrieval_config.bm25_top_k = nonzero_control("bm25_top_k", bm25_top_k)?;
    }
    if let Some(rerank) = req.rerank {
        rerank_config.enabled = rerank;
        if rerank && rerank_config.top_n == 0 {
            rerank_config.top_n = config.rerank.top_n;
        }
    }
    if let Some(rerank_top_n) = req.rerank_top_n {
        rerank_config.top_n = rerank_top_n;
        if rerank_top_n == 0 {
            rerank_config.enabled = false;
        }
    }

    Ok(EffectiveRetrieveControls {
        limit: nonzero_control("limit", req.limit.unwrap_or(config.retrieval.default_limit))?,
        page_size: nonzero_control(
            "page_size",
            req.page_size.unwrap_or(config.retrieval.default_page_size),
        )?,
        page: nonzero_control("page", req.page.unwrap_or(1))?,
        include_debug: req.include_debug,
        include_debug_packs: req.include_debug_packs,
        include_locator: req.include_locator,
        passage: req.passage,
        bypass_cache: req.bypass_cache,
        fast: req.fast,
        config: config.clone(),
        retrieval_config,
        rerank_config,
    })
}

async fn resolve_query_scope(
    state: &SharedState,
    source_id: Option<SourceId>,
    collection_filter: CollectionFilterRequest,
    embedding_profile_id: Option<EmbeddingProfileId>,
) -> Result<QueryScope, (StatusCode, Json<ErrorResponse>)> {
    if !collection_filter.has_filters() {
        return Ok(QueryScope {
            source_filter: source_id.clone().map(single_source_set),
            source_id,
            collection_filter: None,
            collection_provenance: HashMap::new(),
        });
    }

    with_task_store_read(state, move |store| {
        resolve_query_scope_from_store(store, source_id, collection_filter, embedding_profile_id)
    })
    .await
    .map_err(collection_filter_error)
}

fn resolve_query_scope_from_store(
    store: &Store,
    source_id: Option<SourceId>,
    requested: CollectionFilterRequest,
    embedding_profile_id: Option<EmbeddingProfileId>,
) -> Result<QueryScope> {
    let collection_names = collection_filter_names(&requested)?;
    let requested_collection_names = collection_names.iter().cloned().collect::<BTreeSet<_>>();
    let mut union_source_ids = HashSet::new();
    let mut applied = Vec::new();
    let mut warnings = Vec::new();
    let mut required_collection_syncs = BTreeSet::new();
    let mut required_source_ingests = BTreeSet::new();
    let mut collection_provenance: HashMap<String, Vec<CollectionResultProvenance>> =
        HashMap::new();

    for name in collection_names {
        validate_collection_name(&name)?;
        let collection = store
            .get_collection(&name)?
            .with_context(|| format!("collection not found: {name}"))?;
        let members = store.list_collection_members(&name)?;
        let mut indexed_member_count = 0usize;
        let mut stale_member_count = 0usize;

        if collection.last_synced_at.is_none() {
            required_collection_syncs.insert(name.clone());
            warnings.push(format!(
                "collection '{name}' has never been synced; retrieval uses materialized membership and does not scan roots"
            ));
        }
        if members.is_empty() {
            warnings.push(format!("collection '{name}' has no materialized members"));
        }

        for member in &members {
            union_source_ids.insert(member.source_id.clone());
            collection_provenance
                .entry(member.source_id.0.clone())
                .or_default()
                .push(collection_result_provenance(member));
            match store.get_source(&member.source_id)? {
                Some(source) if source.status == SourceStatus::Indexed => {
                    let vectors_stale = embedding_profile_id
                        .as_ref()
                        .map(|profile_id| {
                            store.source_vectors_stale_for_profile(profile_id, &source.id)
                        })
                        .transpose()?
                        .unwrap_or(false);
                    if vectors_stale {
                        stale_member_count += 1;
                        required_source_ingests.insert(source.id.0.clone());
                        tracing::debug!(
                            collection = %name,
                            source_id = %source.id.0,
                            profile_id = embedding_profile_id
                                .as_ref()
                                .map(|profile_id| profile_id.as_str())
                                .unwrap_or(""),
                            "collection member source vectors are stale for the retrieval profile"
                        );
                    } else {
                        indexed_member_count += 1;
                    }
                }
                Some(source) => {
                    stale_member_count += 1;
                    required_source_ingests.insert(source.id.0.clone());
                    tracing::debug!(
                        collection = %name,
                        source_id = %source.id.0,
                        status = source_status_name(&source.status),
                        "collection member source is not indexed"
                    );
                }
                None => {
                    stale_member_count += 1;
                    required_collection_syncs.insert(name.clone());
                }
            }
        }

        if stale_member_count > 0 {
            warnings.push(format!(
                "collection '{name}' has {stale_member_count} member source(s) that are not currently indexed; retrieval does not run indexing automatically"
            ));
        }

        let stale = collection.last_synced_at.is_none() || stale_member_count > 0;
        applied.push(AppliedCollectionFilterResponse {
            collection_id: collection.name.clone(),
            name: collection.name,
            member_count: members.len(),
            indexed_member_count,
            stale_member_count,
            last_synced_at: collection.last_synced_at,
            stale,
        });
    }

    let stale = applied.iter().any(|collection| collection.stale);
    if requested.require_fresh && stale {
        bail!(collection_freshness_remediation_error(
            &requested_collection_names,
            &required_collection_syncs,
            &required_source_ingests,
        ));
    }

    let effective_source_filter = match &source_id {
        Some(source_id) if union_source_ids.contains(source_id) => {
            Some(single_source_set(source_id.clone()))
        }
        Some(source_id) => {
            warnings.push(format!(
                "source '{}' is not a member of the selected collection filter",
                source_id.0
            ));
            Some(HashSet::new())
        }
        None => Some(union_source_ids.clone()),
    };

    let response = CollectionFilterResponse {
        requested,
        union_source_count: union_source_ids.len(),
        applied,
        warnings,
        stale,
    };

    Ok(QueryScope {
        source_id,
        source_filter: effective_source_filter,
        collection_filter: Some(response),
        collection_provenance,
    })
}

fn collection_filter_names(filter: &CollectionFilterRequest) -> Result<Vec<String>> {
    let mut names = BTreeSet::new();
    for raw_name in filter.collection_ids.iter().chain(&filter.names) {
        let name = raw_name.trim();
        if name.is_empty() {
            bail!("collection filter values must not be empty");
        }
        validate_collection_name(name)?;
        names.insert(name.to_string());
    }
    Ok(names.into_iter().collect())
}

fn collection_freshness_remediation_error(
    requested_collection_names: &BTreeSet<String>,
    required_collection_syncs: &BTreeSet<String>,
    required_source_ingests: &BTreeSet<String>,
) -> String {
    const MAX_SOURCE_COMMANDS: usize = 25;

    let mut message = String::from(
        "collection filter requires fresh collection membership and member indexes.\n\nRun the relevant remediation command(s), then retry the query:",
    );

    if required_collection_syncs.is_empty() && required_source_ingests.is_empty() {
        message.push_str("\n  verbatim collection sync <name>");
        message.push_str("\n  verbatim reindex --stale");
        append_collection_retry_command(&mut message, requested_collection_names);
        return message;
    }

    for name in required_collection_syncs {
        message.push_str(&format!("\n  verbatim collection sync {}", shell_arg(name)));
    }

    for source_id in required_source_ingests.iter().take(MAX_SOURCE_COMMANDS) {
        message.push_str(&format!("\n  verbatim ingest {}", shell_arg(source_id)));
    }

    if required_source_ingests.len() > MAX_SOURCE_COMMANDS {
        let omitted = required_source_ingests.len() - MAX_SOURCE_COMMANDS;
        message.push_str(&format!(
            "\n  # {omitted} more stale member source(s) omitted; to rebuild every stale source, run:"
        ));
        message.push_str("\n  verbatim reindex --stale");
    } else if !required_source_ingests.is_empty() {
        message.push_str("\n  # To rebuild every stale source instead, run:");
        message.push_str("\n  verbatim reindex --stale");
    }

    append_collection_retry_command(&mut message, requested_collection_names);
    message
}

fn append_collection_retry_command(
    message: &mut String,
    requested_collection_names: &BTreeSet<String>,
) {
    if requested_collection_names.is_empty() {
        return;
    }

    let collection_args = requested_collection_names
        .iter()
        .map(|name| format!(" --collection {}", shell_arg(name)))
        .collect::<String>();
    message.push_str(&format!(
        "\n\nAfter the command(s) complete, retry:\n  verbatim ask{collection_args} --require-fresh '<question>'"
    ));
}

fn shell_arg(value: &str) -> String {
    if value.bytes().all(|byte| {
        byte.is_ascii_alphanumeric() || matches!(byte, b'.' | b'_' | b'-' | b':' | b'/')
    }) {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "'\"'\"'"))
    }
}

fn collection_result_provenance(member: &CollectionMember) -> CollectionResultProvenance {
    CollectionResultProvenance {
        collection_id: member.collection_name.clone(),
        name: member.collection_name.clone(),
        logical_path: member.logical_path.clone(),
        source_path: member.source_path.display().to_string(),
        member_updated_at: member.updated_at.clone(),
    }
}

fn single_source_set(source_id: SourceId) -> HashSet<SourceId> {
    let mut source_ids = HashSet::new();
    source_ids.insert(source_id);
    source_ids
}

fn collection_filter_error(error: anyhow::Error) -> (StatusCode, Json<ErrorResponse>) {
    let status = if error.to_string().contains("collection not found") {
        StatusCode::NOT_FOUND
    } else {
        StatusCode::BAD_REQUEST
    };
    err(status, error)
}

fn source_status_name(status: &SourceStatus) -> &'static str {
    match status {
        SourceStatus::Pending => "Pending",
        SourceStatus::Indexed => "Indexed",
        SourceStatus::Stale => "Stale",
    }
}

fn nonzero_control(name: &str, value: usize) -> Result<usize> {
    if value == 0 {
        bail!("{name} must be greater than zero");
    }
    Ok(value)
}

fn ingest_source_batch_claim_limit(config: &Config) -> usize {
    config
        .embedding
        .batch_size
        .max(1)
        .saturating_mul(
            config
                .embedding
                .endpoint_runtime
                .bounded()
                .max_concurrent_requests,
        )
        .max(1)
}

fn schedule_ingest_queue(state: SharedState) {
    if state.ingest_queue_active.swap(true, Ordering::AcqRel) {
        return;
    }

    tokio::spawn(async move {
        drain_ingest_queue(Arc::clone(&state)).await;
        state.ingest_queue_active.store(false, Ordering::Release);
        #[cfg(test)]
        if let Some(receipt) = state.ingest_queue_drain_receipt.lock().unwrap().take() {
            let _ = receipt.send(());
        }
        match ingest_queue_ready_to_drain(&state).await {
            Ok(true) => schedule_ingest_queue(state),
            Ok(false) => {}
            Err(err) => tracing::error!(error = %err, "failed to inspect ingest queue"),
        }
    });
}

struct IngestWorkerLease {
    state: SharedState,
}

impl Drop for IngestWorkerLease {
    fn drop(&mut self) {
        self.state
            .ingest_worker_active
            .store(false, Ordering::Release);
        schedule_ingest_queue(Arc::clone(&self.state));
    }
}

fn acquire_ingest_worker(
    state: &SharedState,
) -> Result<IngestWorkerLease, (StatusCode, Json<ErrorResponse>)> {
    state
        .ingest_worker_active
        .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
        .map_err(|_| {
            err(
                StatusCode::CONFLICT,
                anyhow::anyhow!(
                    "ingest queue busy: another ingest task is running; retry later or use `verbatim ingest --background` to queue persistent work"
                ),
            )
        })?;
    Ok(IngestWorkerLease {
        state: Arc::clone(state),
    })
}

async fn drain_ingest_queue(state: SharedState) {
    match recover_abandoned_running_source_batch_children(&state).await {
        Ok(0) => {}
        Ok(recovered) => {
            tracing::warn!(
                recovered,
                "recovered abandoned running source-batch ingest tasks before draining queue"
            );
        }
        Err(err) => {
            tracing::error!(error = %err, "failed to recover abandoned running ingest tasks");
            return;
        }
    }

    loop {
        match expand_next_unexpanded_ingest_batch(&state).await {
            Ok(_) => {}
            Err(err) => {
                tracing::error!(error = %err, "failed to expand queued ingest batch");
                break;
            }
        }
        let work = match claim_startable_ingest_work(&state).await {
            Ok(Some(work)) => work,
            Ok(None) => break,
            Err(err) => {
                tracing::error!(error = %err, "failed to claim queued ingest task");
                break;
            }
        };
        match work {
            ClaimedIngestWork::Single(task) => {
                let task = *task;
                let task_id = task.id.clone();
                let request = match parse_persisted_ingest_request(task.request) {
                    Ok(request) => request,
                    Err(err) => {
                        let _ = finish_task_failed(&state, &task_id, &err.to_string()).await;
                        continue;
                    }
                };
                let controls = IndexingTaskControls {
                    source_id: request.source_id,
                    force: request.force,
                    embedding_profile_id: request.embedding_profile_id,
                    vectors_only: request.vectors_only,
                    ingest_batch_id: request.ingest_batch_id,
                };
                let result = if request.operation.as_deref() == Some("reindex") {
                    execute_started_reindex_task(Arc::clone(&state), &task_id, controls)
                        .await
                        .map(|_| ())
                } else {
                    execute_started_ingest_task(Arc::clone(&state), &task_id, controls)
                        .await
                        .map(|_| ())
                };
                if let Err((_, Json(error))) = &result {
                    let _ = finish_task_failed_from_response(&state, &task_id, error).await;
                }
            }
            ClaimedIngestWork::SourceBatch(tasks) => {
                execute_started_ingest_source_batch(Arc::clone(&state), tasks).await;
            }
        }
    }
}

async fn recover_abandoned_running_source_batch_children(state: &SharedState) -> Result<usize> {
    if state.ingest_worker_active.load(Ordering::Acquire) {
        return Ok(0);
    }
    let recovered = with_task_store_write(
        state,
        recover_abandoned_running_source_batch_children_in_store,
    )
    .await?;
    mark_idle_reclaim_activity_if_changed(state, recovered > 0);
    Ok(recovered)
}

/// Fail all ingest tasks stuck in `running` status from a previous daemon
/// session.  On startup no worker is active, so any `running` task is an
/// orphan that will permanently block the ingest queue (#182) or prevent
/// idle exit by keeping `active_tasks > 0`.
async fn recover_orphaned_running_tasks(state: &SharedState) -> Result<usize> {
    let recovered = with_task_store_write(state, |store| {
        let mut candidates = Vec::new();
        for task in store.tasks_all()? {
            if task.status == TaskStatus::Running {
                candidates.push(task);
            }
        }

        let mut recovered = 0;
        for task in candidates {
            let kind = task.kind;
            let error_message = match kind {
                TaskKind::Ingest => {
                    "orphaned running ingest task from previous daemon session; \
                 no active worker can resume it — failing to unblock ingest queue"
                }
                _ => {
                    "orphaned running task from previous daemon session; \
                 no active worker can resume it — failing to unblock idle exit"
                }
            };
            let resumability = task_failure_resumability_metadata(&task, Some(error_message))
                .ok()
                .flatten();
            let terminalize_timing = PhaseTiming::start(IngestTaskStage::TaskTerminalize.as_str());
            if !store.finish_task_failed_with_result(
                &task.id,
                error_message,
                resumability.as_ref(),
            )? {
                continue;
            }

            let mut payload = serde_json::json!({
                "error": bounded_error(error_message),
                "recovery": "orphaned_running_task_on_startup",
            });
            if let Some(resumability) = &resumability {
                payload["resumability"] = resumability.clone();
            }
            store.insert_task_event(&task.id, "failed", "task failed", &payload)?;
            record_ingest_task_terminalize_span(
                store,
                &task.id,
                terminalize_timing,
                "recover_orphaned_running_task",
            );
            finalize_ingest_batch_parent_if_complete(store, Some(&task))?;
            recovered += 1;
        }
        Ok(recovered)
    })
    .await?;
    mark_idle_reclaim_activity_if_changed(state, recovered > 0);
    Ok(recovered)
}

fn recover_abandoned_running_source_batch_children_in_store(store: &mut Store) -> Result<usize> {
    let mut candidates = Vec::new();
    for task in store.tasks(TaskKind::Ingest)? {
        if task.status == TaskStatus::Running && parse_source_batch_child_request(&task)?.is_some()
        {
            candidates.push(task);
        }
    }

    let mut recovered = 0;
    for task in candidates {
        let error_message = "abandoned running source-batch ingest task recovered without an active worker; failing task to unblock ingest queue";
        let resumability = task_failure_resumability_metadata(&task, Some(error_message))?;
        let terminalize_timing = PhaseTiming::start(IngestTaskStage::TaskTerminalize.as_str());
        if !store.finish_task_failed_with_result(&task.id, error_message, resumability.as_ref())? {
            continue;
        }

        let mut payload = serde_json::json!({
            "error": bounded_error(error_message),
            "recovery": "abandoned_running_source_batch_child",
        });
        if let Some(resumability) = &resumability {
            payload["resumability"] = resumability.clone();
        }
        store.insert_task_event(&task.id, "failed", "task failed", &payload)?;
        record_ingest_task_terminalize_span(
            store,
            &task.id,
            terminalize_timing,
            "recover_abandoned_source_batch_child",
        );
        finalize_ingest_batch_parent_if_complete(store, Some(&task))?;
        recovered += 1;
    }
    Ok(recovered)
}

async fn claim_startable_ingest_work(state: &SharedState) -> Result<Option<ClaimedIngestWork>> {
    if state.ingest_worker_active.load(Ordering::Acquire) {
        return Ok(None);
    }
    let config = runtime_config_snapshot(state)?.config;
    let batch_limit = ingest_source_batch_claim_limit(&config);
    with_task_store_write(state, move |store| {
        claim_next_queue_claimable_ingest_work(store, batch_limit)
    })
    .await
}

async fn ingest_queue_ready_to_drain(state: &SharedState) -> Result<bool> {
    if state.ingest_worker_active.load(Ordering::Acquire) {
        return Ok(false);
    }
    with_task_store_read(state, |store| {
        Ok::<_, anyhow::Error>(
            store.count_running_tasks(TaskKind::Ingest)? == 0
                && (next_unexpanded_ingest_batch_parent(store)?.is_some()
                    || next_queue_claimable_ingest_task(store)?.is_some()),
        )
    })
    .await
}

async fn expand_next_unexpanded_ingest_batch(state: &SharedState) -> Result<bool> {
    if state.ingest_worker_active.load(Ordering::Acquire) {
        return Ok(false);
    }

    let Some(candidate) = next_unexpanded_ingest_batch_parent_async(state).await? else {
        return Ok(false);
    };

    let expansion =
        match background_ingest_batch_sources(state, candidate.force, candidate.vectors_only).await
        {
            Ok(expansion) => expansion,
            Err(error) => {
                finish_task_failed(state, &candidate.task_id, &error.to_string())
                    .await
                    .map_err(|(_, Json(error))| anyhow::anyhow!(error.error))?;
                return Ok(true);
            }
        };

    persist_ingest_batch_children(state, candidate, expansion).await
}

async fn next_unexpanded_ingest_batch_parent_async(
    state: &SharedState,
) -> Result<Option<IngestBatchExpansionCandidate>> {
    with_task_store_read(state, next_unexpanded_ingest_batch_parent).await
}

async fn persist_ingest_batch_children(
    state: &SharedState,
    candidate: IngestBatchExpansionCandidate,
    expansion: BackgroundIngestBatchExpansion,
) -> Result<bool> {
    let persisted = with_task_store_write(state, move |store| {
        if store.count_running_tasks(TaskKind::Ingest)? > 0 {
            return Ok(false);
        }
        let Some(parent) = store.get_task(&candidate.task_id)? else {
            return Ok(false);
        };
        let Some(candidate) = ingest_batch_expansion_candidate(store, &parent)? else {
            return Ok(false);
        };

        for source in &expansion.sources {
            let child_id = TaskId::new();
            let child_request = ingest_task_request_metadata_with_source_hash(
                ingest_task_request_metadata_with_queue_claim_and_batch(
                    Some(source.source_id.0.as_str()),
                    false,
                    candidate.embedding_profile_id.as_deref(),
                    candidate.vectors_only,
                    true,
                    Some(&candidate.task_id.0),
                ),
                source.source_hash.as_deref(),
            );
            let child = store.create_task(&child_id, TaskKind::Ingest, &child_request)?;
            let payload = queued_event_payload(store, child)?;
            store.insert_task_event(&child_id, "queued", "task queued", &payload)?;
        }

        for source_id in &expansion.skipped_missing_sources {
            persist_skipped_missing_ingest_child(
                store,
                &candidate.task_id,
                candidate.embedding_profile_id.as_deref(),
                candidate.vectors_only,
                source_id,
            )?;
        }

        if expansion.sources.is_empty() && expansion.skipped_missing_sources.is_empty() {
            let result = ingest_result_metadata(0, &EmbeddingCacheStats::default());
            let terminalize_timing = PhaseTiming::start(IngestTaskStage::TaskTerminalize.as_str());
            if store.finish_task_success(&candidate.task_id, &result)? {
                store.insert_task_event(
                    &candidate.task_id,
                    "succeeded",
                    "task succeeded",
                    &result,
                )?;
                record_ingest_task_terminalize_span(
                    store,
                    &candidate.task_id,
                    terminalize_timing,
                    "expand_empty_ingest_batch_parent",
                );
            }
            return Ok(true);
        }

        store.insert_task_event(
            &candidate.task_id,
            "batch_expanded",
            "ingest batch children queued",
            &serde_json::json!({
                "ingest_batch_id": candidate.task_id.0.as_str(),
                "children": expansion.sources.len() + expansion.skipped_missing_sources.len(),
                "source_children": expansion.sources.len(),
                "skipped_missing_sources": expansion.skipped_missing_sources.len(),
            }),
        )?;

        if expansion.sources.is_empty() {
            let child = ingest_batch_children(store, &candidate.task_id.0)?
                .into_iter()
                .find(|task| task.status == TaskStatus::Succeeded);
            finalize_ingest_batch_parent_if_complete(store, child.as_ref())?;
            return Ok(true);
        }
        Ok::<_, anyhow::Error>(true)
    })
    .await?;
    mark_idle_reclaim_activity_if_changed(state, persisted);
    Ok(persisted)
}

fn persist_skipped_missing_ingest_child(
    store: &mut Store,
    parent_id: &TaskId,
    embedding_profile_id: Option<&str>,
    vectors_only: bool,
    source_id: &SourceId,
) -> Result<()> {
    let child_id = TaskId::new();
    let child_request = ingest_task_request_metadata_with_queue_claim_and_batch(
        Some(source_id.0.as_str()),
        false,
        embedding_profile_id,
        vectors_only,
        false,
        Some(&parent_id.0),
    );
    let child = store.create_task(&child_id, TaskKind::Ingest, &child_request)?;
    let payload = queued_event_payload(store, child)?;
    store.insert_task_event(&child_id, "queued", "task queued", &payload)?;
    let result = ingest_result_metadata_with_skips(0, &EmbeddingCacheStats::default(), 1);
    if store.finish_task_success(&child_id, &result)? {
        store.insert_task_event(
            &child_id,
            "skipped",
            "source skipped: missing file",
            &serde_json::json!({
                "source_id": source_id.0.as_str(),
                "reason": "missing_source",
                "ingest_batch_id": parent_id.0.as_str(),
            }),
        )?;
        store.insert_task_event(&child_id, "succeeded", "task succeeded", &result)?;
    }
    Ok(())
}

fn parse_persisted_ingest_request(request: serde_json::Value) -> Result<PersistedIngestRequest> {
    let request: PersistedIngestRequest =
        serde_json::from_value(request).context("parse queued ingest request")?;
    if request.ingest_request_version != Some(1) {
        bail!("queued ingest task is missing resumable request metadata; resubmit ingest");
    }
    if !request.queue_claimable.unwrap_or(true) {
        bail!("foreground ingest task is not claimable by the background queue");
    }
    validate_ingest_controls(
        request.source_id.as_deref(),
        request.force,
        request.embedding_profile_id.as_deref(),
        request.vectors_only,
    )?;
    Ok(request)
}

fn resumable_task_plan(task: &verbatim_core::task::TaskSummary) -> Result<Option<ResumeTaskPlan>> {
    if task.kind != TaskKind::Ingest {
        return Ok(None);
    }
    let request: PersistedIngestRequest = serde_json::from_value(task.request.clone())
        .context("parse resumable ingest task request")?;
    if request.ingest_request_version != Some(1) {
        return Ok(None);
    }
    let operation = match request.operation.as_deref().unwrap_or("ingest") {
        "ingest" => ResumableIngestOperation::Ingest,
        "reindex" => ResumableIngestOperation::Reindex,
        _ => return Ok(None),
    };
    validate_ingest_controls(
        request.source_id.as_deref(),
        request.force,
        request.embedding_profile_id.as_deref(),
        request.vectors_only,
    )?;
    Ok(Some(ResumeTaskPlan {
        task_id: task.id.clone(),
        operation,
        controls: IndexingTaskControls {
            source_id: request.source_id,
            force: request.force,
            embedding_profile_id: request.embedding_profile_id,
            vectors_only: request.vectors_only,
            ingest_batch_id: request.ingest_batch_id,
        },
        queue_claimable: request.queue_claimable.unwrap_or(false),
    }))
}

fn next_queue_claimable_ingest_task(
    store: &Store,
) -> Result<Option<verbatim_core::task::TaskSummary>> {
    for task in store.queued_tasks(TaskKind::Ingest)? {
        if ingest_task_can_be_claimed_by_queue(&task.request) {
            if let Some(batch_id) = cancelled_parent_batch_id(store, &task)? {
                cancel_ingest_batch_child(store, &task.id, &batch_id)?;
                continue;
            }
            return Ok(Some(task));
        }
    }
    Ok(None)
}

fn next_unexpanded_ingest_batch_parent(
    store: &Store,
) -> Result<Option<IngestBatchExpansionCandidate>> {
    if store.count_running_tasks(TaskKind::Ingest)? > 0 {
        return Ok(None);
    }
    for task in store.queued_tasks(TaskKind::Ingest)? {
        if let Some(candidate) = ingest_batch_expansion_candidate(store, &task)? {
            return Ok(Some(candidate));
        }
    }
    Ok(None)
}

fn ingest_batch_expansion_candidate(
    store: &Store,
    task: &verbatim_core::task::TaskSummary,
) -> Result<Option<IngestBatchExpansionCandidate>> {
    if task.status != TaskStatus::Queued {
        return Ok(None);
    }
    let Some(request) = parse_ingest_request_lossy(&task.request) else {
        return Ok(None);
    };
    if request.operation.as_deref().unwrap_or("ingest") != "ingest" {
        return Ok(None);
    }
    if request.source_id.is_some() {
        return Ok(None);
    }
    if request.queue_claimable.unwrap_or(true) {
        return Ok(None);
    }
    if request.ingest_batch_id.as_deref() != Some(task.id.0.as_str()) {
        return Ok(None);
    }
    if !ingest_batch_children(store, &task.id.0)?.is_empty() {
        return Ok(None);
    }
    Ok(Some(IngestBatchExpansionCandidate {
        task_id: task.id.clone(),
        force: request.force,
        embedding_profile_id: request.embedding_profile_id,
        vectors_only: request.vectors_only,
    }))
}

fn claim_next_queue_claimable_ingest_task(
    store: &Store,
) -> Result<Option<verbatim_core::task::TaskSummary>> {
    let Some(task) = next_queue_claimable_ingest_task(store)? else {
        return Ok(None);
    };
    if !store.start_task_if_no_running(&task.id, TaskKind::Ingest)? {
        return Ok(None);
    }
    store.insert_task_event(&task.id, "started", "task started", &serde_json::json!({}))?;
    store
        .get_task(&task.id)?
        .with_context(|| format!("claimed ingest task disappeared: {}", task.id.0))
        .map(Some)
}

fn claim_next_queue_claimable_ingest_work(
    store: &Store,
    batch_limit: usize,
) -> Result<Option<ClaimedIngestWork>> {
    let Some(task) = next_queue_claimable_ingest_task(store)? else {
        return Ok(None);
    };
    let source_batch = claimable_source_batch_tasks(store, &task, batch_limit)?;
    if source_batch.len() > 1 {
        let task_ids = source_batch
            .iter()
            .map(|task| task.id.clone())
            .collect::<Vec<_>>();
        if !store.start_tasks_if_no_running(&task_ids, TaskKind::Ingest)? {
            return Ok(None);
        }
        let mut started = Vec::with_capacity(task_ids.len());
        for task_id in task_ids {
            store.insert_task_event(&task_id, "started", "task started", &serde_json::json!({}))?;
            let task = store
                .get_task(&task_id)?
                .with_context(|| format!("claimed ingest task disappeared: {}", task_id.0))?;
            started.push(task);
        }
        return Ok(Some(ClaimedIngestWork::SourceBatch(started)));
    }
    claim_next_queue_claimable_ingest_task(store)
        .map(|task| task.map(|task| ClaimedIngestWork::Single(Box::new(task))))
}

fn claimable_source_batch_tasks(
    store: &Store,
    first: &verbatim_core::task::TaskSummary,
    batch_limit: usize,
) -> Result<Vec<verbatim_core::task::TaskSummary>> {
    let Some(first_request) = parse_source_batch_child_request(first)? else {
        return Ok(vec![first.clone()]);
    };
    let batch_id = first_request
        .ingest_batch_id
        .clone()
        .with_context(|| format!("ingest batch child missing batch id: {}", first.id.0))?;
    let mut tasks = Vec::new();
    for task in store.queued_tasks(TaskKind::Ingest)? {
        if tasks.len() >= batch_limit.max(1) {
            break;
        }
        let Some(request) = parse_source_batch_child_request(&task)? else {
            if task.id == first.id {
                tasks.push(task);
            }
            continue;
        };
        if request.ingest_batch_id.as_deref() != Some(batch_id.as_str()) {
            continue;
        }
        if request.force != first_request.force
            || request.embedding_profile_id != first_request.embedding_profile_id
            || request.vectors_only != first_request.vectors_only
            || request.operation != first_request.operation
        {
            continue;
        }
        if let Some(cancelled_batch_id) = cancelled_parent_batch_id(store, &task)? {
            cancel_ingest_batch_child(store, &task.id, &cancelled_batch_id)?;
            continue;
        }
        tasks.push(task);
    }
    Ok(tasks)
}

fn parse_source_batch_child_request(
    task: &verbatim_core::task::TaskSummary,
) -> Result<Option<PersistedIngestRequest>> {
    let Some(request) = parse_ingest_request_lossy(&task.request) else {
        return Ok(None);
    };
    if request.ingest_request_version != Some(1)
        || !request.queue_claimable.unwrap_or(true)
        || request.operation.as_deref().unwrap_or("ingest") != "ingest"
        || request.source_id.is_none()
        || request.vectors_only
        || request.ingest_batch_id.as_deref() == Some(task.id.0.as_str())
        || request.ingest_batch_id.is_none()
    {
        return Ok(None);
    }
    Ok(Some(request))
}

fn ingest_task_can_be_claimed_by_queue(request: &serde_json::Value) -> bool {
    match serde_json::from_value::<PersistedIngestRequest>(request.clone()) {
        Ok(request) => {
            request.ingest_request_version != Some(1) || request.queue_claimable.unwrap_or(true)
        }
        Err(_) => true,
    }
}

fn cancelled_parent_batch_id(
    store: &Store,
    task: &verbatim_core::task::TaskSummary,
) -> Result<Option<String>> {
    let Some(batch_id) = ingest_request_batch_id(&task.request) else {
        return Ok(None);
    };
    if batch_id == task.id.0 {
        return Ok(None);
    }
    let Some(parent) = store.get_task(&TaskId(batch_id.clone()))? else {
        return Ok(None);
    };
    Ok((parent.status == TaskStatus::Cancelled).then_some(batch_id))
}

fn validate_ingest_controls(
    source_id: Option<&str>,
    force: bool,
    embedding_profile_id: Option<&str>,
    vectors_only: bool,
) -> Result<()> {
    if source_id.is_some() && force {
        bail!("force is only supported for all-source ingest");
    }
    if vectors_only && force {
        bail!("force is not supported for vectors-only embedding profile builds");
    }
    if embedding_profile_id.is_some() && !vectors_only {
        bail!(
            "embedding_profile_id is supported for vectors-only builds; set [embedding].profile_id for parse ingest"
        );
    }
    Ok(())
}

fn resolve_reindex_controls(req: ReindexRequest) -> Result<IndexingTaskControls> {
    let explicit_target_count =
        usize::from(req.source_id.is_some()) + usize::from(req.all) + usize::from(req.stale);
    if explicit_target_count > 1 {
        bail!("choose exactly one reindex target: source_id, all, or stale");
    }
    if req.force && req.source_id.is_some() {
        bail!("force is only supported for all-source reindex");
    }
    if req.force && req.stale {
        bail!("force is not supported with stale reindex");
    }

    let vectors_only = req.vectors_only || req.embedding_profile_id.is_some();
    if req.stale && vectors_only {
        bail!(
            "stale vector-only reindex is not supported; rebuild all vectors or run stale reindex without vectors_only"
        );
    }
    if vectors_only && req.force {
        bail!("force is not supported for vectors-only reindex");
    }
    if explicit_target_count == 0 && !req.force && !vectors_only {
        bail!(
            "reindex requires source_id, all, stale, force, vectors_only, or embedding_profile_id"
        );
    }

    let force = if vectors_only {
        false
    } else {
        req.all || req.force
    };
    Ok(IndexingTaskControls {
        source_id: req.source_id,
        force,
        embedding_profile_id: req.embedding_profile_id,
        vectors_only,
        ingest_batch_id: None,
    })
}

async fn validate_requested_source_exists(
    state: &SharedState,
    source_id: Option<&str>,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let Some(source_id) = source_id else {
        return Ok(());
    };
    let source_id = source_id.to_string();
    let lookup_id = SourceId(source_id.clone());
    let found = with_task_store_read(state, move |store| store.get_source(&lookup_id))
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?
        .is_some();
    if found {
        Ok(())
    } else {
        Err(err(
            StatusCode::NOT_FOUND,
            anyhow::anyhow!("source not found: {source_id}"),
        ))
    }
}

async fn execute_ingest_task(
    state: SharedState,
    task_id: TaskId,
    controls: IndexingTaskControls,
) -> Result<IngestResponse, (StatusCode, Json<ErrorResponse>)> {
    let result = async {
        start_foreground_ingest_task(&state, &task_id).await?;
        execute_started_ingest_task(Arc::clone(&state), &task_id, controls).await
    }
    .await;
    if let Err((_, Json(error))) = &result {
        let _ = finish_task_failed_from_response(&state, &task_id, error).await;
    }
    result
}

async fn execute_started_ingest_task(
    state: SharedState,
    task_id: &TaskId,
    controls: IndexingTaskControls,
) -> Result<IngestResponse, (StatusCode, Json<ErrorResponse>)> {
    let (outcome, profile_id) =
        run_indexing_operation(Arc::clone(&state), task_id, &controls, "ingest").await?;
    let response = IngestResponse {
        ingested: outcome.source_count,
    };
    finish_task_success(
        &state,
        task_id,
        ingest_result_metadata_with_skips(
            response.ingested,
            &outcome.embedding_cache,
            outcome.skipped_missing_sources,
        ),
    )
    .await?;
    tracing::debug!(
        task_id = %task_id.0,
        embedding_profile_id = %profile_id,
        "ingest task completed"
    );
    Ok(response)
}

async fn execute_started_ingest_source_batch(
    state: SharedState,
    tasks: Vec<verbatim_core::task::TaskSummary>,
) {
    if tasks.is_empty() {
        return;
    }
    let source_tasks = match started_source_batch_inputs(&tasks) {
        Ok(source_tasks) => source_tasks,
        Err(error) => {
            for task in tasks {
                let _ = finish_task_failed(&state, &task.id, &error.to_string()).await;
            }
            return;
        }
    };
    match run_started_ingest_source_batch(Arc::clone(&state), source_tasks).await {
        Ok(outcomes) => {
            if let Err(error) =
                finish_started_ingest_source_batch_outcomes(&state, tasks, outcomes).await
            {
                tracing::error!(error = %error, "failed to finalize source batch ingest tasks");
            }
        }
        Err((_, Json(error))) => {
            for task in tasks {
                let _ = finish_task_failed_from_response(&state, &task.id, &error).await;
            }
        }
    }
}

async fn finish_started_ingest_source_batch_outcomes(
    state: &SharedState,
    tasks: Vec<verbatim_core::task::TaskSummary>,
    outcomes: Vec<SourceIngestOutcome>,
) -> Result<()> {
    let mut outcomes_by_task_id = HashMap::with_capacity(outcomes.len());
    let mut first_error = None;
    for outcome in outcomes {
        let task_id = outcome.task_id.clone();
        if outcomes_by_task_id
            .insert(task_id.clone(), outcome)
            .is_some()
        {
            first_error.get_or_insert_with(|| {
                anyhow::anyhow!("duplicate source batch outcome for task {}", task_id.0)
            });
        }
    }

    for task in tasks {
        match outcomes_by_task_id.remove(&task.id) {
            Some(outcome) => {
                if let Err(error) = finish_source_batch_task_outcome(state, outcome).await {
                    tracing::error!(task_id = %task.id.0, error = %error, "failed to mark source batch task terminal");
                    first_error.get_or_insert(error);
                }
            }
            None => {
                let error_message = format!(
                    "missing source batch outcome for started task {}; failing task to unblock ingest queue",
                    task.id.0
                );
                if let Err((status, Json(error))) =
                    finish_task_failed(state, &task.id, &error_message).await
                {
                    tracing::error!(task_id = %task.id.0, status = %status, error = %error.error, "failed to mark source batch task failed after missing outcome");
                    first_error.get_or_insert_with(|| {
                        anyhow::anyhow!(
                            "failed to mark source batch task {} failed after missing outcome: {}: {}",
                            task.id.0,
                            status,
                            error.error
                        )
                    });
                }
            }
        }
    }

    for task_id in outcomes_by_task_id.into_keys() {
        tracing::warn!(
            task_id = %task_id.0,
            "source batch returned outcome for a task that was not claimed in this batch"
        );
    }

    first_error.map_or(Ok(()), Err)
}

async fn finish_source_batch_task_outcome(
    state: &SharedState,
    outcome: SourceIngestOutcome,
) -> Result<()> {
    let task_id = outcome.task_id.clone();
    match outcome.result {
        Ok(embedding_cache) => {
            let result = ingest_result_metadata(1, &embedding_cache);
            finish_task_success(state, &task_id, result).await.map_err(
                |(status, Json(error))| {
                    anyhow::anyhow!(
                        "failed to mark source batch task {} succeeded: {}: {}",
                        task_id.0,
                        status,
                        error.error
                    )
                },
            )?;
        }
        Err(error_message) => {
            finish_task_failed(state, &task_id, &error_message)
                .await
                .map_err(|(status, Json(error))| {
                    anyhow::anyhow!(
                        "failed to mark source batch task {} failed: {}: {}",
                        task_id.0,
                        status,
                        error.error
                    )
                })?;
        }
    }
    Ok(())
}

fn started_source_batch_inputs(
    tasks: &[verbatim_core::task::TaskSummary],
) -> Result<Vec<(SourceId, TaskId)>> {
    tasks
        .iter()
        .map(|task| {
            let request = parse_persisted_ingest_request(task.request.clone())?;
            if request.operation.as_deref().unwrap_or("ingest") != "ingest" {
                bail!("source batch execution only supports ingest tasks");
            }
            if request.vectors_only {
                bail!("source batch execution does not support vectors-only tasks");
            }
            let source_id = request
                .source_id
                .with_context(|| format!("source batch task missing source id: {}", task.id.0))?;
            Ok((SourceId(source_id), task.id.clone()))
        })
        .collect()
}

async fn run_started_ingest_source_batch(
    state: SharedState,
    source_tasks: Vec<(SourceId, TaskId)>,
) -> Result<Vec<SourceIngestOutcome>, (StatusCode, Json<ErrorResponse>)> {
    let _worker = acquire_ingest_worker(&state)?;
    let runtime = tokio::runtime::Handle::current();
    let state_for_reporter = Arc::clone(&state);
    let state_for_pipeline = Arc::clone(&state);
    let (result, index_status) = tokio::task::spawn_blocking(move || {
        run_with_pipeline(state_for_pipeline, move |pipeline| {
            let state_for_reporter = Arc::clone(&state_for_reporter);
            let result = runtime.block_on(pipeline.ingest_sources_with_tasks_reporting(
                &source_tasks,
                move |outcome| {
                    let state = Arc::clone(&state_for_reporter);
                    async move {
                        if let Err(error) = finish_source_batch_task_outcome(&state, outcome).await {
                            tracing::error!(error = %error, "failed to stream-finalize source batch task outcome");
                        }
                    }
                },
            ));
            let index_status = initial_index_status_cache(pipeline);
            Ok((result, index_status))
        })
    })
    .await
    .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error.into()))?
    .map_err(pipeline_access_error)?;
    if let Some(index_status) = index_status {
        if let Err(error) = update_index_status_cache(&state, &index_status) {
            tracing::warn!(error = %error, "failed to update index status cache after source batch");
        }
    }
    Ok(result)
}

async fn execute_reindex_task(
    state: SharedState,
    task_id: TaskId,
    controls: IndexingTaskControls,
) -> Result<ReindexResponse, (StatusCode, Json<ErrorResponse>)> {
    let result = async {
        start_foreground_ingest_task(&state, &task_id).await?;
        execute_started_reindex_task(Arc::clone(&state), &task_id, controls).await
    }
    .await;
    if let Err((_, Json(error))) = &result {
        let _ = finish_task_failed_from_response(&state, &task_id, error).await;
    }
    result
}

async fn execute_started_reindex_task(
    state: SharedState,
    task_id: &TaskId,
    controls: IndexingTaskControls,
) -> Result<ReindexResponse, (StatusCode, Json<ErrorResponse>)> {
    let (outcome, profile_id) =
        run_indexing_operation(Arc::clone(&state), task_id, &controls, "reindex").await?;
    let response = ReindexResponse {
        reindexed: outcome.source_count,
    };
    finish_task_success(
        &state,
        task_id,
        reindex_result_metadata(response.reindexed, &outcome.embedding_cache),
    )
    .await?;
    tracing::debug!(
        task_id = %task_id.0,
        embedding_profile_id = %profile_id,
        "reindex task completed"
    );
    Ok(response)
}

async fn run_indexing_operation(
    state: SharedState,
    task_id: &TaskId,
    controls: &IndexingTaskControls,
    phase_name: &str,
) -> Result<(IndexingOutcome, EmbeddingProfileId), (StatusCode, Json<ErrorResponse>)> {
    if controls.embedding_profile_id.is_some() && !controls.vectors_only {
        return Err(err(
            StatusCode::BAD_REQUEST,
            anyhow::anyhow!(
                "embedding_profile_id is supported for vectors-only builds; set [embedding].profile_id for parse ingest"
            ),
        ));
    }
    let config = runtime_config_snapshot(&state)
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?
        .config;
    let profile_id = parse_embedding_profile_id(
        controls.embedding_profile_id.as_deref(),
        &config.embedding.profile_id,
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let write_operation =
        sqlite_durability_ops::preflight_indexing_capacity(&state, controls).await?;
    let _worker = acquire_ingest_worker(&state)?;
    let task_id2 = task_id.clone();
    let profile_id_for_task = profile_id.clone();
    let source_id = controls.source_id.clone();
    let source_id_for_error = controls.source_id.clone();
    let force = controls.force;
    let vectors_only = controls.vectors_only;
    let timing = PhaseTiming::start(phase_name);
    record_task_progress(
        &state,
        task_id,
        timing
            .progress_snapshot()
            .with_counter("sources", 0, controls.source_id.as_ref().map(|_| 1_u64))
            .with_recent_status(format!("{phase_name} started"))
            .with_active_worker_kind(TaskKind::Ingest.as_str()),
    )
    .await;
    let (outcome, index_status) = sqlite_durability_ops::run_indexing_with_pipeline(
        Arc::clone(&state),
        vectors_only,
        source_id,
        profile_id_for_task,
        task_id2,
        force,
    )
    .await?;
    if let Some(index_status) = index_status {
        if let Err(error) = update_index_status_cache(&state, &index_status) {
            tracing::warn!(error = %error, "failed to update index status cache after indexing operation");
        }
    }
    let outcome = outcome.map_err(|error| {
        sqlite_durability_ops::indexing_operation_error(
            source_id_for_error.as_deref(),
            write_operation,
            error,
        )
    })?;
    let mut progress = timing
        .progress_snapshot()
        .with_counter(
            "sources",
            outcome.source_count as u64,
            controls.source_id.as_ref().map(|_| 1_u64),
        )
        .with_counter(
            "embedding_cache_hits",
            outcome.embedding_cache.cache_hits as u64,
            None,
        )
        .with_counter(
            "embedding_cache_misses",
            outcome.embedding_cache.cache_misses as u64,
            None,
        )
        .with_counter(
            "embedded_chunks",
            outcome.embedding_cache.embedded_chunks as u64,
            None,
        )
        .with_counter(
            "reused_chunks",
            outcome.embedding_cache.reused_chunks as u64,
            None,
        )
        .with_counter(
            "changed_chunks",
            outcome.embedding_cache.changed_chunks as u64,
            None,
        )
        .with_recent_status(format!("{phase_name} complete"))
        .with_active_worker_kind(TaskKind::Ingest.as_str());
    let finished = timing.finish(serde_json::json!({
        "rebuilt": outcome.source_count,
        "force": controls.force,
        "embedding_profile_id": profile_id.as_str(),
        "vectors_only": controls.vectors_only,
        "source_id": controls.source_id.as_deref(),
        "ingest_batch_id": controls.ingest_batch_id.as_deref(),
        "embedding_cache": &outcome.embedding_cache,
    }));
    if controls.vectors_only {
        progress.set_endpoint(TaskEndpointSummary::single_call(
            "embedding",
            finished.duration_ms,
        ));
    }
    record_task_progress(&state, task_id, progress).await;
    record_task_span(&state, task_id, finished).await?;
    Ok((outcome, profile_id))
}

async fn cancel_task_record(
    state: &SharedState,
    task_id: &TaskId,
) -> Result<bool, (StatusCode, Json<ErrorResponse>)> {
    let task_id = task_id.clone();
    let changed = with_task_store_write(state, move |store| {
        let task = store
            .get_task(&task_id)?
            .with_context(|| format!("task not found: {}", task_id.0))?;
        let is_ingest_task = task.kind == TaskKind::Ingest;
        let terminalize_timing =
            is_ingest_task.then(|| PhaseTiming::start(IngestTaskStage::TaskTerminalize.as_str()));
        let changed = store.cancel_task(&task_id)?;
        if changed {
            store.insert_task_event(
                &task_id,
                "cancelled",
                "task cancelled",
                &serde_json::json!({}),
            )?;
            if let Some(batch_id) = task_controlled_ingest_batch_id(&task) {
                let cancelled_children =
                    cancel_active_ingest_batch_children(store, &task_id, &batch_id)?;
                store.insert_task_event(
                    &task_id,
                    "batch_cancelled",
                    "ingest batch children cancelled",
                    &serde_json::json!({
                        "ingest_batch_id": batch_id,
                        "cancelled_children": cancelled_children,
                    }),
                )?;
            }
            if let Some(timing) = terminalize_timing {
                record_ingest_task_terminalize_span(store, &task_id, timing, "cancel_task");
            }
        }
        Ok(changed)
    })
    .await
    .map_err(|e| {
        if e.to_string().contains("task not found") {
            err(StatusCode::NOT_FOUND, e)
        } else {
            err(StatusCode::INTERNAL_SERVER_ERROR, e)
        }
    })?;
    mark_idle_reclaim_activity_if_changed(state, changed);
    Ok(changed)
}

fn task_controlled_ingest_batch_id(task: &verbatim_core::task::TaskSummary) -> Option<String> {
    if task.kind != TaskKind::Ingest {
        return None;
    }
    let request = parse_ingest_request_lossy(&task.request)?;
    let batch_id = request.ingest_batch_id?;
    (batch_id == task.id.0).then_some(batch_id)
}

fn parse_ingest_request_lossy(request: &serde_json::Value) -> Option<PersistedIngestRequest> {
    serde_json::from_value(request.clone()).ok()
}

fn cancel_active_ingest_batch_children(
    store: &Store,
    parent_id: &TaskId,
    batch_id: &str,
) -> Result<usize> {
    let mut cancelled = 0;
    for task in store.active_tasks(TaskKind::Ingest)? {
        if task.id == *parent_id {
            continue;
        }
        if ingest_request_batch_id(&task.request).as_deref() != Some(batch_id) {
            continue;
        }
        if cancel_ingest_batch_child(store, &task.id, batch_id)? {
            cancelled += 1;
        }
    }
    Ok(cancelled)
}

fn cancel_ingest_batch_child(store: &Store, task_id: &TaskId, batch_id: &str) -> Result<bool> {
    let terminalize_timing = PhaseTiming::start(IngestTaskStage::TaskTerminalize.as_str());
    let changed = store.cancel_task(task_id)?;
    if changed {
        store.insert_task_event(
            task_id,
            "cancelled",
            "task cancelled because ingest batch was cancelled",
            &serde_json::json!({ "ingest_batch_id": batch_id }),
        )?;
        record_ingest_task_terminalize_span(
            store,
            task_id,
            terminalize_timing,
            "cancel_ingest_batch_child",
        );
    }
    Ok(changed)
}

fn ingest_request_batch_id(request: &serde_json::Value) -> Option<String> {
    parse_ingest_request_lossy(request)?.ingest_batch_id
}

fn task_child_ingest_batch_id(task: &verbatim_core::task::TaskSummary) -> Option<String> {
    if task.kind != TaskKind::Ingest {
        return None;
    }
    let batch_id = ingest_request_batch_id(&task.request)?;
    (batch_id != task.id.0).then_some(batch_id)
}

fn ingest_batch_children(
    store: &Store,
    batch_id: &str,
) -> Result<Vec<verbatim_core::task::TaskSummary>> {
    let children = store
        .tasks(TaskKind::Ingest)?
        .into_iter()
        .filter(|task| {
            task.id.0 != batch_id
                && ingest_request_batch_id(&task.request).as_deref() == Some(batch_id)
        })
        .collect::<Vec<_>>();
    Ok(children)
}

fn finalize_ingest_batch_parent_if_complete(
    store: &Store,
    completed_task: Option<&verbatim_core::task::TaskSummary>,
) -> Result<()> {
    let Some(batch_id) = completed_task.and_then(task_child_ingest_batch_id) else {
        return Ok(());
    };
    let parent_id = TaskId(batch_id.clone());
    let Some(parent) = store.get_task(&parent_id)? else {
        return Ok(());
    };
    if parent.status.is_terminal() {
        return Ok(());
    }

    let children = ingest_batch_children(store, &batch_id)?;
    if children.is_empty() || children.iter().any(|task| !task.status.is_terminal()) {
        return Ok(());
    }

    let terminalize_timing = PhaseTiming::start(IngestTaskStage::TaskTerminalize.as_str());
    let result = ingest_batch_result_metadata(&batch_id, &children);
    let has_failed_or_cancelled = children
        .iter()
        .any(|task| matches!(task.status, TaskStatus::Failed | TaskStatus::Cancelled));
    if has_failed_or_cancelled {
        let error = "one or more ingest batch children did not succeed";
        if store.finish_task_failed_with_result(&parent_id, error, Some(&result))? {
            store.insert_task_event(
                &parent_id,
                "failed",
                "task failed",
                &serde_json::json!({
                    "error": bounded_error(error),
                    "ingest_batch": result,
                }),
            )?;
            record_ingest_task_terminalize_span(
                store,
                &parent_id,
                terminalize_timing,
                "finalize_ingest_batch_parent_failed",
            );
        }
        return Ok(());
    }

    if store.finish_task_success(&parent_id, &result)? {
        store.insert_task_event(&parent_id, "succeeded", "task succeeded", &result)?;
        record_ingest_task_terminalize_span(
            store,
            &parent_id,
            terminalize_timing,
            "finalize_ingest_batch_parent_success",
        );
    }
    Ok(())
}

fn ingest_batch_result_metadata(
    batch_id: &str,
    children: &[verbatim_core::task::TaskSummary],
) -> serde_json::Value {
    let mut embedding_cache = EmbeddingCacheStats::default();
    let mut ingested = 0;
    let mut succeeded = 0;
    let mut failed = 0;
    let mut cancelled = 0;
    let mut skipped_missing_sources = 0;

    for child in children {
        match child.status {
            TaskStatus::Succeeded => {
                succeeded += 1;
                if let Some(result) = child.result.as_ref() {
                    ingested += result
                        .get("ingested")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|value| usize::try_from(value).ok())
                        .unwrap_or(1);
                    skipped_missing_sources += result
                        .get("skipped_missing_sources")
                        .and_then(serde_json::Value::as_u64)
                        .and_then(|value| usize::try_from(value).ok())
                        .unwrap_or_default();
                } else {
                    ingested += 1;
                }
                if let Some(stats) = child
                    .result
                    .as_ref()
                    .and_then(|result| result.get("embedding_cache"))
                    .cloned()
                    .and_then(|value| serde_json::from_value::<EmbeddingCacheStats>(value).ok())
                {
                    embedding_cache.add(&stats);
                }
            }
            TaskStatus::Failed => failed += 1,
            TaskStatus::Cancelled => cancelled += 1,
            TaskStatus::Queued | TaskStatus::Running => {}
        }
    }

    let mut result = ingest_result_metadata(ingested, &embedding_cache);
    if let serde_json::Value::Object(map) = &mut result {
        map.insert(
            "ingest_batch_id".into(),
            serde_json::Value::String(batch_id.into()),
        );
        map.insert("total_children".into(), serde_json::json!(children.len()));
        map.insert("succeeded_children".into(), serde_json::json!(succeeded));
        map.insert("failed_children".into(), serde_json::json!(failed));
        map.insert("cancelled_children".into(), serde_json::json!(cancelled));
        map.insert(
            "skipped_missing_sources".into(),
            serde_json::json!(skipped_missing_sources),
        );
    }
    result
}

async fn task_wait_snapshot(
    state: &SharedState,
    task_id: TaskId,
    after: Option<i64>,
    limit: usize,
) -> Result<TaskWaitEvent, (StatusCode, Json<ErrorResponse>)> {
    with_task_store_read(state, move |store| {
        let task = store
            .get_task(&task_id)?
            .with_context(|| format!("task not found: {}", task_id.0))?;
        let redaction = task_telemetry_redaction(store, &task)?;
        let task = with_queue_details(store, task)?;
        let events = store
            .list_task_events(&task_id, after, limit)?
            .into_iter()
            .map(|event| public_task_event_for_source(event, &redaction))
            .collect();
        let spans = if task.status.is_terminal() {
            store
                .list_task_spans(&task_id)?
                .into_iter()
                .map(|span| public_task_span(span, &redaction))
                .collect()
        } else {
            Vec::new()
        };
        let terminal = task.status.is_terminal();
        Ok::<_, anyhow::Error>(TaskWaitEvent {
            task: public_task_summary(task, &redaction),
            events,
            spans,
            terminal,
        })
    })
    .await
    .map_err(|e| {
        if e.to_string().contains("task not found") {
            err(StatusCode::NOT_FOUND, e)
        } else {
            err(StatusCode::INTERNAL_SERVER_ERROR, e)
        }
    })
}

fn try_send_stream_event(tx: &mpsc::Sender<Event>, event: Event) -> Result<()> {
    match tx.try_send(event) {
        Ok(()) => Ok(()),
        Err(mpsc::error::TrySendError::Full(_)) => {
            bail!("client is not keeping up during ask stream")
        }
        Err(mpsc::error::TrySendError::Closed(_)) => bail!("client disconnected during ask stream"),
    }
}

async fn send_stream_event(
    tx: &mpsc::Sender<Event>,
    event: Event,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    tx.send(event).await.map_err(|_| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            anyhow::anyhow!("client disconnected"),
        )
    })
}

fn parse_embedding_profile_id(
    requested: Option<&str>,
    default_profile_id: &EmbeddingProfileId,
) -> Result<EmbeddingProfileId> {
    requested
        .map(EmbeddingProfileId::try_from)
        .transpose()
        .map(|profile_id| profile_id.unwrap_or_else(|| default_profile_id.clone()))
        .map_err(Into::into)
}

async fn refresh_query_embedding_profile_capabilities(
    state: &SharedState,
    embedding_enabled: bool,
    embedding_profile_id: &EmbeddingProfileId,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    refresh_live_embedding_profile_capabilities(state, embedding_enabled, embedding_profile_id)
        .await
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))
}

async fn refresh_live_embedding_profile_capabilities(
    state: &SharedState,
    embedding_enabled: bool,
    embedding_profile_id: &EmbeddingProfileId,
) -> Result<()> {
    if !embedding_enabled {
        return Ok(());
    }
    let config = runtime_config_snapshot(state)?.config;
    let embed_client = OpenAiEmbeddingClient::new(&config.embedding);
    let capabilities = embed_client.endpoint_capabilities().await?;
    apply_query_embedding_profile_capabilities(state, capabilities, embedding_profile_id.clone())
        .await
}

async fn apply_query_embedding_profile_capabilities(
    state: &SharedState,
    capabilities: EmbeddingEndpointCapabilities,
    embedding_profile_id: EmbeddingProfileId,
) -> Result<()> {
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || match PipelineLease::take(state) {
        Ok(mut lease) => {
            let result = match lease.pipeline.as_mut() {
                Some(pipeline) => {
                    pipeline.apply_embedding_profile_capabilities(capabilities)?;
                    pipeline.select_embedding_profile(&embedding_profile_id)?;
                    Ok(())
                }
                None => Err(pipeline_busy_error()),
            };
            let restore = lease.restore();
            restore.and(result)
        }
        Err(error) if is_pipeline_busy_error(&error) => Ok(()),
        Err(error) => Err(error),
    })
    .await
    .context("join query embedding profile capability apply task")?
}

fn prepare_query_embedding_profile_readonly(
    pipeline: &mut IngestPipeline,
    embedding_enabled: bool,
    embedding_profile_id: &EmbeddingProfileId,
) -> Result<()> {
    if embedding_enabled {
        pipeline.select_embedding_profile_readonly(embedding_profile_id)?;
    }
    Ok(())
}

async fn refresh_query_embedding_profile_for_collection_filter(
    state: &SharedState,
    collection_filter: &CollectionFilterRequest,
    embedding_enabled: bool,
    embedding_profile_id: &EmbeddingProfileId,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    if !collection_filter.has_filters() || !embedding_enabled {
        return Ok(());
    }

    let embedding_profile_id = embedding_profile_id.clone();
    refresh_query_embedding_profile_capabilities(state, embedding_enabled, &embedding_profile_id)
        .await
}

async fn prepare_retrieve_context(
    state: SharedState,
    question: &str,
    source_filter: Option<HashSet<SourceId>>,
    embedding_profile_id: &EmbeddingProfileId,
    controls: &EffectiveRetrieveControls,
) -> Result<RetrievedContext, (StatusCode, Json<ErrorResponse>)> {
    refresh_query_embedding_profile_capabilities(
        &state,
        controls.config.embedding.enabled,
        embedding_profile_id,
    )
    .await?;
    let question2 = question.to_string();
    let embedding_profile_id = embedding_profile_id.clone();
    let controls = controls.clone();
    let sqlite_reader = Arc::clone(&state.resources.sqlite_reader);
    let vector_search = Arc::clone(&state.resources.vector_search);
    let runtime = tokio::runtime::Handle::current();
    with_query_pipeline(&state, move |pipeline| {
        with_sqlite_reader_permit(&sqlite_reader, || {
            prepare_query_embedding_profile_readonly(
                pipeline,
                controls.config.embedding.enabled,
                &embedding_profile_id,
            )
        })?;
        let lexical_index = pipeline.lexical_index();
        let embed_client = OpenAiEmbeddingClient::new(&controls.config.embedding);
        let retrieval = RetrievalPipeline::new_with_graph(
            pipeline.vector_index(),
            &lexical_index,
            pipeline.store(),
            &embed_client,
            &controls.retrieval_config,
            &controls.config.graph,
        )
        .with_embedding_enabled(controls.config.embedding.enabled)
        .with_vector_residency(controls.config.vector_index.residency)
        .require_embedding_profile(&embedding_profile_id)
        .with_prefix_cache_bypass(controls.bypass_cache)
        .with_read_resource(Arc::clone(&sqlite_reader))
        .with_vector_search_resource(vector_search)
        .with_qdrant_search(&controls.config.qdrant);
        let source_filter_ref = source_filter.as_ref();
        let debug_options = retrieve_debug_options(&controls);
        let (retrieval_result, retrieval_search_sql_statement_count, retrieval_resource_counters) =
            pipeline.store().measure_retrieval(|| {
                let (mut results, mut debug) = match (
                    controls.rerank_config.enabled,
                    controls.rerank_config.strategy,
                ) {
                    (true, RerankStrategy::Endpoint) => {
                        let reranker =
                            OpenAiCompatibleReranker::from_config(&controls.rerank_config);
                        let retrieval = retrieval.with_reranker(&controls.rerank_config, &reranker);
                        runtime.block_on(retrieval.search_source_set_with_debug_options(
                            &question2,
                            source_filter_ref,
                            debug_options,
                        ))?
                    }
                    (true, RerankStrategy::Llm) => {
                        let reranker =
                            OpenAiCompatibleLlmReranker::from_config(&controls.rerank_config);
                        let retrieval = retrieval.with_reranker(&controls.rerank_config, &reranker);
                        runtime.block_on(retrieval.search_source_set_with_debug_options(
                            &question2,
                            source_filter_ref,
                            debug_options,
                        ))?
                    }
                    (false, _) => {
                        runtime.block_on(retrieval.search_source_set_with_debug_options(
                            &question2,
                            source_filter_ref,
                            debug_options,
                        ))?
                    }
                };
                let global_results = with_sqlite_reader_permit(&sqlite_reader, || {
                    GraphRagService::new(pipeline.store(), &controls.config.graph.global_search)
                        .global_search_backing_results(&question2, source_filter_ref)
                })?;
                prepend_global_backing_results(&mut results, global_results, Some(&mut debug));
                Ok::<_, anyhow::Error>((results, debug))
            });
        let (results, mut debug) = retrieval_result?;
        debug.retrieval_search_sql_statement_count = retrieval_search_sql_statement_count;
        debug.retrieval_resource_counters = retrieval_resource_counters;
        let source_paths = with_sqlite_reader_permit(&sqlite_reader, || {
            source_paths_for_results(&results, pipeline.store())
        })?;
        Ok::<_, anyhow::Error>(RetrievedContext {
            results,
            debug,
            source_paths,
        })
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))
}

#[cfg(test)]
fn empty_retrieval_debug() -> RetrievalDebug {
    RetrievalDebug {
        dense_vector_path: RetrievalDenseVectorPath::Bm25Only,
        query_embedding_latency_ms: None,
        retrieval_search_sql_statement_count: None,
        retrieval_resource_counters: None,
        local_spans_ms: RetrievalLocalSpansMs::default(),
        candidate_counters: Default::default(),
        evidence_pack_mode: verbatim_core::types::RetrievalDebugEvidencePackMode::Full,
        final_evidence_count: 0,
        display_evidence_count: 0,
        bm25_hits: Vec::new(),
        dense_hits: Vec::new(),
        rrf_fused_hits: Vec::new(),
        graph_expanded_hits: Vec::new(),
        reranker: verbatim_core::types::RetrievalRerankDebug::disabled(),
        final_evidence_pack: Vec::new(),
        display_evidence_pack: Vec::new(),
    }
}

async fn prepare_generation_context(
    state: SharedState,
    question: &str,
    source_filter: Option<HashSet<SourceId>>,
    embedding_profile_id: &EmbeddingProfileId,
    config: &Config,
    show_retrieval: bool,
) -> Result<
    (
        Vec<RetrievalResult>,
        GenerationContext,
        Option<RetrievalDebug>,
    ),
    (StatusCode, Json<ErrorResponse>),
> {
    // Retrieve first; this touches the !Send store and async embed client.
    refresh_query_embedding_profile_capabilities(
        &state,
        config.embedding.enabled,
        embedding_profile_id,
    )
    .await?;
    let question2 = question.to_string();
    let embedding_profile_id = embedding_profile_id.clone();
    let config = config.clone();
    let data_dir = state.data_dir.clone();
    let sqlite_reader = Arc::clone(&state.resources.sqlite_reader);
    let vector_search = Arc::clone(&state.resources.vector_search);
    let runtime = tokio::runtime::Handle::current();
    let (results, generation_context, retrieval_debug) =
        with_query_pipeline(&state, move |pipeline| {
            with_sqlite_reader_permit(&sqlite_reader, || {
                prepare_query_embedding_profile_readonly(
                    pipeline,
                    config.embedding.enabled,
                    &embedding_profile_id,
                )
            })?;
            let lexical_index = pipeline.lexical_index();
            let embed_client = OpenAiEmbeddingClient::new(&config.embedding);
            let retrieval = RetrievalPipeline::new_with_graph(
                pipeline.vector_index(),
                &lexical_index,
                pipeline.store(),
                &embed_client,
                &config.retrieval,
                &config.graph,
            )
            .with_embedding_enabled(config.embedding.enabled)
            .with_vector_residency(config.vector_index.residency)
            .require_embedding_profile(&embedding_profile_id)
            .with_read_resource(Arc::clone(&sqlite_reader))
            .with_vector_search_resource(vector_search)
            .with_qdrant_search(&config.qdrant);
            let source_filter_ref = source_filter.as_ref();
            let (
                retrieval_result,
                retrieval_search_sql_statement_count,
                retrieval_resource_counters,
            ) = pipeline.store().measure_retrieval(|| {
                let (mut results, mut retrieval_debug) = run_generation_retrieval(
                    runtime,
                    retrieval,
                    &config,
                    &question2,
                    source_filter_ref,
                    show_retrieval,
                )?;
                let global_results = with_sqlite_reader_permit(&sqlite_reader, || {
                    GraphRagService::new(pipeline.store(), &config.graph.global_search)
                        .global_search_backing_results(&question2, source_filter_ref)
                })?;
                prepend_global_backing_results(
                    &mut results,
                    global_results,
                    retrieval_debug.as_mut(),
                );
                Ok::<_, anyhow::Error>((results, retrieval_debug))
            });
            let (results, mut retrieval_debug) = retrieval_result?;
            if let Some(debug) = retrieval_debug.as_mut() {
                debug.retrieval_search_sql_statement_count = retrieval_search_sql_statement_count;
                debug.retrieval_resource_counters = retrieval_resource_counters;
            }
            let image_artifacts = with_sqlite_reader_permit(&sqlite_reader, || {
                collect_image_artifacts_for_results(&results, pipeline.store())
            })?;
            let image_attachments = select_image_attachments(
                &results,
                &image_artifacts,
                &config.chat.vision_attachments,
                |artifact| read_image_attachment_bytes(&data_dir, artifact),
            )?;
            Ok::<_, anyhow::Error>((
                results,
                GenerationContext::new(image_artifacts, image_attachments),
                retrieval_debug,
            ))
        })
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok((results, generation_context, retrieval_debug))
}

fn ask_debug_options(config: &Config, show_retrieval: bool) -> RetrievalDebugOptions {
    let canonical_budget =
        RetrievalCanonicalSelectionBudget::scoped(RetrievalDisplayScope::Window {
            start: 0,
            len: config.retrieval.default_limit,
        });
    if show_retrieval {
        RetrievalDebugOptions::full(canonical_budget)
    } else {
        RetrievalDebugOptions::compact(canonical_budget)
    }
}

fn run_generation_retrieval(
    runtime: tokio::runtime::Handle,
    retrieval: RetrievalPipeline<'_>,
    config: &Config,
    question: &str,
    source_filter: Option<&HashSet<SourceId>>,
    show_retrieval: bool,
) -> Result<(Vec<RetrievalResult>, Option<RetrievalDebug>)> {
    let rerank_config = &config.rerank;
    let debug_options = ask_debug_options(config, show_retrieval);
    match (rerank_config.enabled, rerank_config.strategy) {
        (true, RerankStrategy::Endpoint) => {
            let reranker = OpenAiCompatibleReranker::from_config(rerank_config);
            run_generation_retrieval_once(
                &runtime,
                retrieval.with_reranker(rerank_config, &reranker),
                question,
                source_filter,
                debug_options,
            )
        }
        (true, RerankStrategy::Llm) => {
            let reranker = OpenAiCompatibleLlmReranker::from_config(rerank_config);
            run_generation_retrieval_once(
                &runtime,
                retrieval.with_reranker(rerank_config, &reranker),
                question,
                source_filter,
                debug_options,
            )
        }
        (false, _) => run_generation_retrieval_once(
            &runtime,
            retrieval,
            question,
            source_filter,
            debug_options,
        ),
    }
}

fn retrieval_span_metadata(
    mut metadata: serde_json::Value,
    debug: Option<&RetrievalDebug>,
) -> serde_json::Value {
    if let (Some(count), Some(fields)) = (
        debug.and_then(|debug| debug.retrieval_search_sql_statement_count),
        metadata.as_object_mut(),
    ) {
        fields.insert("retrieval_search_sql_statement_count".into(), count.into());
    }
    if let (Some(counters), Some(fields)) = (
        debug.and_then(|debug| debug.retrieval_resource_counters.as_ref()),
        metadata.as_object_mut(),
    ) {
        fields.insert(
            "retrieval_resource_counters".into(),
            serde_json::json!(counters),
        );
    }
    metadata
}

fn run_generation_retrieval_once(
    runtime: &tokio::runtime::Handle,
    retrieval: RetrievalPipeline<'_>,
    question: &str,
    source_filter: Option<&HashSet<SourceId>>,
    debug_options: RetrievalDebugOptions,
) -> Result<(Vec<RetrievalResult>, Option<RetrievalDebug>)> {
    let (results, debug) = runtime.block_on(retrieval.search_source_set_with_debug_options(
        question,
        source_filter,
        debug_options,
    ))?;
    Ok((results, Some(debug)))
}

fn prepend_global_backing_results(
    results: &mut Vec<RetrievalResult>,
    mut global_results: Vec<RetrievalResult>,
    retrieval_debug: Option<&mut RetrievalDebug>,
) {
    if global_results.is_empty() {
        return;
    }

    global_results.extend(std::mem::take(results));
    *results = global_results;
    for (index, result) in results.iter_mut().enumerate() {
        result.provenance.result_rank = index + 1;
    }
    if let Some(debug) = retrieval_debug {
        refresh_evidence_pack_debug(debug, results);
    }
}

fn collect_image_artifacts_for_results(
    results: &[RetrievalResult],
    store: &Store,
) -> Result<Vec<ImageArtifact>> {
    let mut artifacts = Vec::new();
    let mut seen = HashSet::new();

    for result in results {
        for evidence in &result.evidence_units {
            let Some(evidence_id) = image_artifact_evidence_id(evidence) else {
                continue;
            };
            if !seen.insert(evidence_id.0.clone()) {
                continue;
            }
            if let Some(artifact) = store.get_image_artifact_by_evidence(evidence_id)? {
                artifacts.push(artifact);
            }
        }
    }

    Ok(artifacts)
}

fn read_image_attachment_bytes(data_dir: &FsPath, artifact: &ImageArtifact) -> Result<Vec<u8>> {
    let path = checked_image_artifact_absolute_path(data_dir, &artifact.relative_path)?;
    fs::read(&path).with_context(|| format!("read image attachment: {}", path.display()))
}

fn checked_image_artifact_absolute_path(
    data_dir: &FsPath,
    relative_path: &FsPath,
) -> Result<PathBuf> {
    ensure_relative_image_artifact_path(relative_path)?;
    let root = data_dir.join("image-artifacts");
    let absolute_path = data_dir.join(relative_path);
    let parent = absolute_path.parent().with_context(|| {
        format!(
            "image artifact path has no parent: {}",
            absolute_path.display()
        )
    })?;
    if absolute_path == data_dir
        || absolute_path == root
        || !absolute_path.starts_with(&root)
        || parent == root
        || !parent.starts_with(&root)
    {
        bail!(
            "unsafe image artifact path outside artifact root: {}",
            absolute_path.display()
        );
    }
    Ok(absolute_path)
}

fn source_paths_for_results(
    results: &[RetrievalResult],
    store: &Store,
) -> Result<HashMap<String, String>> {
    let mut paths = HashMap::new();
    for result in results {
        for evidence in &result.evidence_units {
            if paths.contains_key(&evidence.source_id.0) {
                continue;
            }
            if let Some(source) = store.get_source(&evidence.source_id)? {
                paths.insert(
                    evidence.source_id.0.clone(),
                    source.path.display().to_string(),
                );
            }
        }
    }
    Ok(paths)
}

fn filter_generated_retrieval_evidence(
    results: &mut Vec<RetrievalResult>,
    debug: &mut RetrievalDebug,
) {
    if !results.iter().any(|result| {
        result
            .evidence_units
            .iter()
            .any(|evidence| evidence.kind == EvidenceKind::Generated)
    }) {
        return;
    }
    for result in results.iter_mut() {
        result
            .evidence_units
            .retain(|evidence| evidence.kind != EvidenceKind::Generated);
    }
    results.retain(|result| !result.evidence_units.is_empty());
    for (index, result) in results.iter_mut().enumerate() {
        result.provenance.result_rank = index + 1;
    }
    refresh_evidence_pack_debug(debug, results);
}

fn retrieve_response(store: &Store, input: RetrieveResponseInput) -> Result<RetrieveResponse> {
    let RetrieveResponseInput {
        task_id,
        query,
        source_filter,
        collection_filter,
        collection_provenance,
        embedding_profile_id,
        controls,
        results,
        debug,
        source_paths,
        retrieval_ms,
    } = input;
    let (total_results, results_page) = if controls.passage {
        retrieve_passage_result_page(RetrieveResultPageInput {
            store,
            results: &results,
            debug: &debug,
            source_paths: &source_paths,
            collection_provenance: &collection_provenance,
            limit: controls.limit,
            page_size: controls.page_size,
            page: controls.page,
            include_locator: controls.include_locator,
        })?
    } else {
        let display_pack = retrieve_display_evidence_pack(&debug);
        let total_results = retrieve_display_evidence_count(&debug, display_pack);
        let results_page = retrieve_result_page(RetrieveResultPageInput {
            store,
            results: &results,
            debug: &debug,
            source_paths: &source_paths,
            collection_provenance: &collection_provenance,
            limit: controls.limit,
            page_size: controls.page_size,
            page: controls.page,
            include_locator: controls.include_locator,
        })?;
        (total_results, results_page)
    };
    let returned_results = results_page.len();
    let debug = controls.include_debug.then_some(debug);

    Ok(RetrieveResponse {
        task_id: task_id.0,
        query,
        source_id: source_filter.map(|source_id| source_id.0),
        collection_filter,
        embedding_profile_id: embedding_profile_id.into_string(),
        limit: controls.limit,
        page_size: controls.page_size,
        page: controls.page,
        total_results,
        returned_results,
        source_bounded: true,
        controls: RetrieveControlsResponse {
            fast: controls.fast,
            rerank_enabled: controls.rerank_config.enabled,
            dense_top_k: controls.retrieval_config.dense_top_k,
            bm25_top_k: controls.retrieval_config.bm25_top_k,
            rrf_k: controls.retrieval_config.rrf_k,
            rerank_top_n: controls.rerank_config.top_n,
        },
        timings: vec![RetrieveTimingResponse {
            phase: "retrieval".into(),
            duration_ms: retrieval_ms,
        }],
        results: results_page,
        debug,
    })
}

fn retrieve_passage_result_page(
    input: RetrieveResultPageInput<'_>,
) -> Result<(usize, Vec<RetrieveResultResponse>)> {
    let RetrieveResultPageInput {
        store,
        results,
        debug: _,
        source_paths,
        collection_provenance,
        limit,
        page_size,
        page,
        include_locator,
    } = input;
    let groups = retrieve_passage_groups(results);
    let total_results = groups.len();
    let end = total_results.min(limit);
    let start = page_start(page, page_size);
    if start >= end {
        return Ok((total_results, Vec::new()));
    }

    let page = groups
        .iter()
        .enumerate()
        .skip(start)
        .take(end - start)
        .take(page_size)
        .map(|(page_index, group)| {
            passage_group_response(PassageGroupResponseInput {
                store,
                group_index: page_index,
                group,
                source_paths,
                collection_provenance,
                include_locator,
            })
        })
        .collect::<Result<Vec<_>>>()?;
    Ok((total_results, page))
}

fn retrieve_display_evidence_pack(debug: &RetrievalDebug) -> &[RetrievalEvidencePackEntry] {
    if debug.display_evidence_pack.is_empty()
        || (!debug.final_evidence_pack.is_empty()
            && debug.display_evidence_pack.len() >= debug.final_evidence_pack.len())
    {
        &debug.final_evidence_pack
    } else {
        &debug.display_evidence_pack
    }
}

fn retrieve_display_evidence_count(
    debug: &RetrievalDebug,
    display_pack: &[RetrievalEvidencePackEntry],
) -> usize {
    if debug.display_evidence_count > 0 || display_pack.is_empty() {
        debug.display_evidence_count
    } else {
        display_pack.len()
    }
}

struct PassageGroup<'a> {
    result: &'a RetrievalResult,
    first_evidence_ordinal: usize,
}

fn retrieve_passage_group_count(results: &[RetrievalResult]) -> usize {
    let mut seen_chunks = HashSet::new();
    results
        .iter()
        .filter(|result| {
            !result.evidence_units.is_empty() && seen_chunks.insert(result.chunk_id.0.clone())
        })
        .count()
}

fn retrieve_passage_groups(results: &[RetrievalResult]) -> Vec<PassageGroup<'_>> {
    let mut groups: Vec<PassageGroup<'_>> = Vec::new();
    let mut seen_chunks = HashSet::new();
    let mut evidence_ordinal = 1usize;

    for result in results {
        if result.evidence_units.is_empty() || !seen_chunks.insert(result.chunk_id.0.clone()) {
            continue;
        }
        groups.push(PassageGroup {
            result,
            first_evidence_ordinal: evidence_ordinal,
        });
        evidence_ordinal = evidence_ordinal.saturating_add(result.evidence_units.len());
    }

    groups
}

struct PassageGroupResponseInput<'a> {
    store: &'a Store,
    group_index: usize,
    group: &'a PassageGroup<'a>,
    source_paths: &'a HashMap<String, String>,
    collection_provenance: &'a HashMap<String, Vec<CollectionResultProvenance>>,
    include_locator: bool,
}

fn passage_group_response(input: PassageGroupResponseInput<'_>) -> Result<RetrieveResultResponse> {
    let PassageGroupResponseInput {
        store,
        group_index,
        group,
        source_paths,
        collection_provenance,
        include_locator,
    } = input;
    let evidence_units = group
        .result
        .evidence_units
        .iter()
        .map(|evidence| store.resolve_source_bounded_evidence(evidence))
        .collect::<Result<Vec<_>>>()?;
    let first = evidence_units
        .first()
        .expect("passage groups are never empty");
    let last = evidence_units
        .last()
        .expect("passage groups are never empty");
    let (locator, structured_locator) = passage_locator(first, last, include_locator);

    Ok(RetrieveResultResponse {
        index: group_index,
        rank: group_index + 1,
        label: format!("E{}", group.first_evidence_ordinal),
        evidence_id: first.id.0.clone(),
        text_hash: first.text_hash.clone(),
        source_id: first.source_id.0.clone(),
        source_path: source_paths.get(&first.source_id.0).cloned(),
        collections: collection_provenance
            .get(&first.source_id.0)
            .cloned()
            .unwrap_or_default(),
        chunk_id: group.result.chunk_id.0.clone(),
        kind: evidence_kind_name(first.kind).to_string(),
        role: retrieval_role_name(retrieval_evidence_role(first)).to_string(),
        score: group.result.score,
        locator,
        structured_locator,
        provenance: include_locator.then(|| group.result.provenance.clone()),
        derived_from: first.derived_from.as_ref().map(|id| id.0.clone()),
        snippet: passage_snippet(&evidence_units),
    })
}

fn passage_locator(
    first: &EvidenceUnit,
    last: &EvidenceUnit,
    include_locator: bool,
) -> (String, Option<SourceLocator>) {
    let range = match (&first.locator, &last.locator) {
        (
            SourceLocator::Canonical { locator: first },
            SourceLocator::Canonical { locator: last },
        ) => Some(canonical_passage_locator(first, last)),
        _ => None,
    };

    if let Some(locator) = range {
        let display = locator.display.clone();
        let structured = include_locator.then_some(SourceLocator::Canonical { locator });
        (display, structured)
    } else {
        (
            first.locator.to_string(),
            include_locator.then(|| first.locator.clone()),
        )
    }
}

fn canonical_passage_locator(
    first: &CanonicalLocator,
    last: &CanonicalLocator,
) -> CanonicalLocator {
    if first == last {
        return first.clone();
    }

    let end = last.end.clone().unwrap_or_else(|| last.start.clone());
    let display = canonical_passage_display(first, last);
    let normalized = if first.normalized == last.normalized {
        first.normalized.clone()
    } else {
        format!("{}-{}", first.normalized, last.normalized)
    };
    let mut locator = CanonicalLocator::range(
        &first.profile_id,
        &first.work_id,
        first.start.clone(),
        end,
        display,
        normalized,
    );
    locator.version_id = first.version_id.clone();
    locator.backing_selectors = first
        .backing_selectors
        .iter()
        .chain(last.backing_selectors.iter())
        .cloned()
        .collect();
    locator
}

fn canonical_passage_display(first: &CanonicalLocator, last: &CanonicalLocator) -> String {
    let last_end = last.end.as_ref().unwrap_or(&last.start);
    let first_book = reference_component_value(&first.start, "book");
    let last_book = reference_component_value(last_end, "book");
    let first_chapter = reference_component_value(&first.start, "chapter");
    let last_chapter = reference_component_value(last_end, "chapter");
    let first_verse = reference_component_value(&first.start, "verse");
    let last_verse = reference_component_value(last_end, "verse");

    match (
        first_book,
        last_book,
        first_chapter,
        last_chapter,
        first_verse,
        last_verse,
    ) {
        (
            Some(book),
            Some(last_book),
            Some(chapter),
            Some(last_chapter),
            Some(verse),
            Some(last_verse),
        ) if book == last_book && chapter == last_chapter => {
            format!("{book} {chapter}:{verse}-{last_verse}")
        }
        (
            Some(book),
            Some(last_book),
            Some(chapter),
            Some(last_chapter),
            Some(verse),
            Some(last_verse),
        ) if book == last_book => {
            format!("{book} {chapter}:{verse}-{last_chapter}:{last_verse}")
        }
        _ => format!("{}-{}", first.display, last.display),
    }
}

fn reference_component_value<'a>(
    components: &'a [ReferenceComponent],
    level: &str,
) -> Option<&'a str> {
    components
        .iter()
        .find(|component| component.level == level)
        .map(|component| component.value.as_str())
}

fn passage_snippet(evidence_units: &[EvidenceUnit]) -> String {
    evidence_units
        .iter()
        .map(|evidence| evidence.text.as_str())
        .map(normalize_inline_text)
        .filter(|text| !text.is_empty())
        .collect::<Vec<_>>()
        .join(" ")
}

fn page_len(total_results: usize, limit: usize, page_size: usize, page: usize) -> usize {
    let end = total_results.min(limit);
    let start = page_start(page, page_size);
    if start >= end {
        0
    } else {
        end.saturating_sub(start).min(page_size)
    }
}

struct RetrieveResultPageInput<'a> {
    store: &'a Store,
    results: &'a [RetrievalResult],
    debug: &'a RetrievalDebug,
    source_paths: &'a HashMap<String, String>,
    collection_provenance: &'a HashMap<String, Vec<CollectionResultProvenance>>,
    limit: usize,
    page_size: usize,
    page: usize,
    include_locator: bool,
}

fn retrieve_result_page(input: RetrieveResultPageInput<'_>) -> Result<Vec<RetrieveResultResponse>> {
    let RetrieveResultPageInput {
        store,
        results,
        debug,
        source_paths,
        collection_provenance,
        limit,
        page_size,
        page,
        include_locator,
    } = input;
    let display_pack = retrieve_display_evidence_pack(debug);
    let start = page_start(page, page_size);
    let end = display_pack.len().min(limit);
    if start >= end {
        return Ok(Vec::new());
    }

    display_pack
        .iter()
        .enumerate()
        .skip(start)
        .take(end - start)
        .take(page_size)
        .map(|(index, entry)| {
            let expected = selected_retrieval_evidence(results, entry)?;
            let evidence = store.resolve_source_bounded_evidence(expected)?;
            Ok(RetrieveResultResponse {
                index,
                rank: index + 1,
                label: entry.label.clone(),
                evidence_id: evidence.id.0.clone(),
                text_hash: evidence.text_hash.clone(),
                source_id: evidence.source_id.0.clone(),
                source_path: source_paths.get(&evidence.source_id.0).cloned(),
                collections: collection_provenance
                    .get(&evidence.source_id.0)
                    .cloned()
                    .unwrap_or_default(),
                chunk_id: entry.chunk_id.0.clone(),
                kind: evidence_kind_name(evidence.kind).to_string(),
                role: retrieval_role_name(retrieval_evidence_role(&evidence)).to_string(),
                score: entry.score,
                locator: evidence.locator.to_string(),
                structured_locator: include_locator.then(|| evidence.locator.clone()),
                provenance: include_locator.then(|| entry.provenance.clone()),
                derived_from: evidence.derived_from.as_ref().map(|id| id.0.clone()),
                snippet: compact_snippet(&evidence.text, DEFAULT_SNIPPET_CHARS),
            })
        })
        .collect()
}

fn selected_retrieval_evidence<'a>(
    results: &'a [RetrievalResult],
    entry: &RetrievalEvidencePackEntry,
) -> Result<&'a EvidenceUnit> {
    results
        .iter()
        .flat_map(|result| &result.evidence_units)
        .find(|evidence| {
            evidence.id == entry.evidence_id
                && evidence.source_id == entry.source_id
                && evidence.kind == entry.kind
                && evidence.derived_from == entry.derived_from
                && evidence.locator == entry.locator.structured
        })
        .with_context(|| {
            format!(
                "source-bounded evidence not found in retrieval snapshot: {}",
                entry.evidence_id.0
            )
        })
}

fn page_start(page: usize, page_size: usize) -> usize {
    page.saturating_sub(1).saturating_mul(page_size)
}

fn normalize_inline_text(text: &str) -> String {
    text.split_whitespace().collect::<Vec<_>>().join(" ")
}

fn compact_snippet(text: &str, max_chars: usize) -> String {
    let normalized = normalize_inline_text(text);
    let mut snippet = String::new();
    for (index, ch) in normalized.chars().enumerate() {
        if index == max_chars {
            snippet.push_str("...");
            return snippet;
        }
        snippet.push(ch);
    }
    snippet
}

fn ensure_relative_image_artifact_path(relative_path: &FsPath) -> Result<()> {
    let components: Vec<Component<'_>> = relative_path.components().collect();
    match components.as_slice() {
        [Component::Normal(root), Component::Normal(source), Component::Normal(file)]
            if root.to_str() == Some("image-artifacts")
                && is_safe_path_component(source)
                && is_safe_path_component(file) =>
        {
            Ok(())
        }
        _ => bail!(
            "unsafe image artifact relative path: {}",
            relative_path.display()
        ),
    }
}

fn is_safe_path_component(component: &std::ffi::OsStr) -> bool {
    component
        .to_str()
        .is_some_and(|text| !text.is_empty() && text != "." && text != "..")
}

async fn get_evidence(
    State(state): State<SharedState>,
    Path(eid): Path<String>,
) -> Result<Json<EvidenceResponse>, (StatusCode, Json<ErrorResponse>)> {
    let eid_clone = eid.clone();
    let (evidence, image_artifact) = with_task_store_read(&state, move |store| {
        let evidence = store
            .get_evidence(&EvidenceId(eid_clone))?
            .map(|evidence| store.resolve_source_bounded_evidence(&evidence))
            .transpose()?;
        let image_artifact = match &evidence {
            Some(eu) => {
                let direct = store.get_image_artifact_by_evidence(&eu.id)?;
                match (direct, &eu.derived_from) {
                    (Some(artifact), _) => Some(artifact),
                    (None, Some(source_evidence_id)) => {
                        store.get_image_artifact_by_evidence(source_evidence_id)?
                    }
                    (None, None) => None,
                }
            }
            None => None,
        };
        Ok::<_, anyhow::Error>((evidence, image_artifact))
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    match evidence {
        Some(eu) => Ok(Json(EvidenceResponse {
            kind: evidence_kind_name(eu.kind).to_string(),
            id: eu.id.0,
            source_id: eu.source_id.0,
            source_bounded: eu.kind != EvidenceKind::Generated,
            text_hash: eu.text_hash,
            derived_from: eu.derived_from.map(|id| id.0),
            locator: eu.locator.to_string(),
            structured_locator: eu.locator,
            text: eu.text,
            heading_path: eu.heading_path,
            position: eu.position,
            image_artifact: image_artifact.map(ImageArtifactResponse::from),
        })),
        None => Err(err(
            StatusCode::NOT_FOUND,
            anyhow::anyhow!("evidence not found: {eid}"),
        )),
    }
}

fn evidence_kind_name(kind: EvidenceKind) -> &'static str {
    match kind {
        EvidenceKind::Text => "text",
        EvidenceKind::Ocr => "ocr",
        EvidenceKind::Image => "image",
        EvidenceKind::Generated => "generated",
    }
}

fn citation_kind_name(citation: &CitationRef) -> &'static str {
    match citation.kind {
        EvidenceKind::Text => "original_text",
        EvidenceKind::Ocr => "ocr_text",
        EvidenceKind::Image => "image_artifact",
        EvidenceKind::Generated if citation.derived_from.is_some() => "image_caption_generated",
        EvidenceKind::Generated => "generated",
    }
}

fn retrieval_role_name(role: RetrievalEvidenceRole) -> &'static str {
    match role {
        RetrievalEvidenceRole::OriginalText => "original_text",
        RetrievalEvidenceRole::OcrText => "ocr_text",
        RetrievalEvidenceRole::ImageArtifact => "image_artifact",
        RetrievalEvidenceRole::ImageCaptionGenerated => "image_caption_generated",
        RetrievalEvidenceRole::Generated => "generated",
    }
}

fn retrieval_evidence_role(evidence: &EvidenceUnit) -> RetrievalEvidenceRole {
    match evidence.kind {
        EvidenceKind::Text => RetrievalEvidenceRole::OriginalText,
        EvidenceKind::Ocr => RetrievalEvidenceRole::OcrText,
        EvidenceKind::Image => RetrievalEvidenceRole::ImageArtifact,
        EvidenceKind::Generated if evidence.derived_from.is_some() => {
            RetrievalEvidenceRole::ImageCaptionGenerated
        }
        EvidenceKind::Generated => RetrievalEvidenceRole::Generated,
    }
}

fn citation_response_with_collections(
    citation: CitationRef,
    collection_provenance: &HashMap<String, Vec<CollectionResultProvenance>>,
) -> CitationResponse {
    let kind = citation_kind_name(&citation);
    let collections = collection_provenance
        .get(&citation.source_id.0)
        .cloned()
        .unwrap_or_default();
    CitationResponse {
        label: citation.label,
        evidence_id: citation.evidence_id.0,
        kind: kind.to_string(),
        derived_from: citation.derived_from.map(|id| id.0),
        collections,
        locator: citation.locator.to_string(),
        text_preview: citation.text_preview,
    }
}

fn sse_json_event<T>(name: &'static str, value: &T) -> Event
where
    T: Serialize,
{
    Event::default()
        .event(name)
        .json_data(value)
        .unwrap_or_else(|error| {
            Event::default()
                .event("error")
                .data(format!("failed to serialize {name} event: {error}"))
        })
}

fn sse_error_event(status: StatusCode, error: impl Into<String>) -> Event {
    sse_json_event(
        "error",
        &AskErrorEvent {
            status: Some(status.as_u16()),
            error: error.into(),
        },
    )
}

// ---------------------------------------------------------------------------
// Collection watcher
// ---------------------------------------------------------------------------

#[derive(Default)]
struct CollectionWatchPlan {
    roots: BTreeMap<PathBuf, CollectionWatchRoot>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum CollectionWatchPathChange {
    Unwatch(PathBuf),
    Watch {
        path: PathBuf,
        recursive: RecursiveMode,
    },
}

#[derive(Clone)]
struct CollectionWatchRoot {
    recursive: RecursiveMode,
    collections: BTreeSet<String>,
}

#[derive(Default)]
struct CollectionMaintenanceOutcome {
    added: usize,
    removed: usize,
    unchanged: usize,
    queued_task_ids: Vec<String>,
}

fn start_collection_watcher(state: SharedState) -> Result<tokio::task::JoinHandle<()>> {
    let (tx, rx) = mpsc::channel(COLLECTION_WATCHER_EVENT_BUFFER);
    set_collection_watcher_sender(&state, tx.clone());
    let callback_tx = tx;
    let watcher = notify::recommended_watcher(move |event: notify::Result<notify::Event>| {
        let command = match event {
            Ok(event) if collection_notify_event_is_relevant(&event.kind) => {
                Some(CollectionWatcherCommand::FilesystemEvent { paths: event.paths })
            }
            Ok(_) => None,
            Err(error) => Some(CollectionWatcherCommand::NotifyError {
                error: error.to_string(),
            }),
        };
        if let Some(command) = command {
            let _ = callback_tx.try_send(command);
        }
    })
    .context("create collection watcher")?;

    Ok(tokio::spawn(async move {
        run_collection_watcher(state, watcher, rx).await;
    }))
}

fn collection_notify_event_is_relevant(kind: &EventKind) -> bool {
    !matches!(kind, EventKind::Access(_))
}

async fn run_collection_watcher(
    state: SharedState,
    mut watcher: RecommendedWatcher,
    mut rx: mpsc::Receiver<CollectionWatcherCommand>,
) {
    let mut plan = CollectionWatchPlan::default();
    let mut watched_paths: BTreeMap<PathBuf, RecursiveMode> = BTreeMap::new();
    if let Err(error) =
        refresh_collection_watches(&state, &mut watcher, &mut watched_paths, &mut plan).await
    {
        tracing::warn!(error = %error, "initial collection watcher refresh failed");
    }

    let mut debounced = DebouncedCollectionSet::default();
    let mut deadline: Option<tokio::time::Instant> = None;

    loop {
        if let Some(instant) = deadline {
            tokio::select! {
                command = rx.recv() => {
                    let Some(command) = command else {
                        break;
                    };
                    handle_collection_watcher_command(
                        &state,
                        &mut watcher,
                        &mut watched_paths,
                        &mut plan,
                        &mut debounced,
                        &mut deadline,
                        command,
                    )
                    .await;
                }
                () = tokio::time::sleep_until(instant) => {
                    flush_collection_watcher_debounce(&state, &mut debounced).await;
                    deadline = None;
                }
            }
        } else {
            let Some(command) = rx.recv().await else {
                break;
            };
            handle_collection_watcher_command(
                &state,
                &mut watcher,
                &mut watched_paths,
                &mut plan,
                &mut debounced,
                &mut deadline,
                command,
            )
            .await;
        }
    }
}

async fn handle_collection_watcher_command(
    state: &SharedState,
    watcher: &mut RecommendedWatcher,
    watched_paths: &mut BTreeMap<PathBuf, RecursiveMode>,
    plan: &mut CollectionWatchPlan,
    debounced: &mut DebouncedCollectionSet,
    deadline: &mut Option<tokio::time::Instant>,
    command: CollectionWatcherCommand,
) {
    match command {
        CollectionWatcherCommand::FilesystemEvent { paths } => {
            let names = collection_names_for_event_paths(plan, &paths);
            if names.is_empty() {
                return;
            }
            let now = unix_timestamp_string();
            for name in &names {
                update_collection_watcher_status(state, name, |status| {
                    status.pending_event_count = status.pending_event_count.saturating_add(1);
                    status.last_event_at = Some(now.clone());
                });
            }
            debounced.insert_many(names);
            let debounce = collection_watcher_debounce(state);
            *deadline = Some(tokio::time::Instant::now() + debounce);
        }
        CollectionWatcherCommand::NotifyError { error } => {
            tracing::warn!(error = %error, "collection watcher notify error");
            mark_active_collection_watchers_error(state, &error);
        }
        CollectionWatcherCommand::Refresh => {
            if let Err(error) =
                refresh_collection_watches(state, watcher, watched_paths, plan).await
            {
                tracing::warn!(error = %error, "collection watcher refresh failed");
                mark_active_collection_watchers_error(state, &error.to_string());
            }
        }
        CollectionWatcherCommand::ResyncActive => {
            resync_active_collection_watchers(state).await;
        }
    }
}

fn collection_watcher_debounce(state: &SharedState) -> Duration {
    runtime_config_snapshot(state)
        .ok()
        .map(|snapshot| snapshot.config.collection_watcher.debounce_millis.max(1))
        .map(Duration::from_millis)
        .unwrap_or(Duration::from_millis(500))
}

async fn flush_collection_watcher_debounce(
    state: &SharedState,
    debounced: &mut DebouncedCollectionSet,
) {
    if debounced.is_empty() {
        return;
    }
    let names = debounced.drain();
    for name in names {
        match maintain_collection_after_watch_event(state, &name).await {
            Ok(outcome) => {
                let last_task_id = outcome.queued_task_ids.last().cloned();
                update_collection_watcher_status(state, &name, |status| {
                    status.pending_event_count = 0;
                    status.last_sync_at = Some(unix_timestamp_string());
                    status.last_error = None;
                    status.last_added = outcome.added;
                    status.last_removed = outcome.removed;
                    status.last_unchanged = outcome.unchanged;
                    if last_task_id.is_some() {
                        status.last_task_id = last_task_id;
                    }
                });
            }
            Err(error) => {
                tracing::warn!(collection = %name, error = %error, "collection watcher maintenance failed");
                record_collection_watcher_error(state, &name, error);
            }
        }
    }
}

async fn resync_active_collection_watchers(state: &SharedState) {
    let names = match state.collection_watcher.statuses.lock() {
        Ok(statuses) => statuses
            .iter()
            .filter(|(_, status)| status.active && status.watched_root_count > 0)
            .map(|(name, _)| name.clone())
            .collect::<Vec<_>>(),
        Err(error) => {
            tracing::warn!(error = %error, "failed to collect active collection watchers for resync");
            Vec::new()
        }
    };
    let mut all_succeeded = true;
    for name in names {
        match maintain_collection_after_watch_event(state, &name).await {
            Ok(outcome) => {
                let last_task_id = outcome.queued_task_ids.last().cloned();
                update_collection_watcher_status(state, &name, |status| {
                    status.last_sync_at = Some(unix_timestamp_string());
                    status.last_error = None;
                    status.last_added = outcome.added;
                    status.last_removed = outcome.removed;
                    status.last_unchanged = outcome.unchanged;
                    if last_task_id.is_some() {
                        status.last_task_id = last_task_id;
                    }
                });
            }
            Err(error) => {
                all_succeeded = false;
                tracing::warn!(collection = %name, error = %error, "collection watcher idle-exit resync failed");
                record_collection_watcher_error(state, &name, error);
            }
        }
    }
    if all_succeeded {
        state
            .idle_exit
            .watcher_resync_requested
            .store(false, Ordering::Release);
    }
}

async fn refresh_collection_watches(
    state: &SharedState,
    watcher: &mut RecommendedWatcher,
    watched_paths: &mut BTreeMap<PathBuf, RecursiveMode>,
    plan: &mut CollectionWatchPlan,
) -> Result<()> {
    let next_plan = build_collection_watch_plan(state).await?;
    for change in collection_watch_path_changes(watched_paths, &next_plan) {
        match change {
            CollectionWatchPathChange::Unwatch(path) => {
                if let Err(error) = watcher.unwatch(&path) {
                    tracing::warn!(path = %path.display(), error = %error, "failed to unwatch collection root");
                }
                watched_paths.remove(&path);
            }
            CollectionWatchPathChange::Watch { path, recursive } => {
                match watcher.watch(&path, recursive) {
                    Ok(()) => {
                        watched_paths.insert(path.clone(), recursive);
                    }
                    Err(error) => {
                        tracing::warn!(path = %path.display(), error = %error, "failed to watch collection root");
                        if let Some(root) = next_plan.roots.get(&path) {
                            for collection in &root.collections {
                                record_collection_watcher_error(state, collection, &error);
                            }
                        }
                    }
                }
            }
        };
    }
    *plan = next_plan;
    Ok(())
}

fn collection_watch_path_changes(
    watched_paths: &BTreeMap<PathBuf, RecursiveMode>,
    next_plan: &CollectionWatchPlan,
) -> Vec<CollectionWatchPathChange> {
    let mut changes = Vec::new();
    for (path, watched_recursive) in watched_paths {
        match next_plan.roots.get(path) {
            Some(root) if root.recursive == *watched_recursive => {}
            Some(root) => {
                changes.push(CollectionWatchPathChange::Unwatch(path.clone()));
                changes.push(CollectionWatchPathChange::Watch {
                    path: path.clone(),
                    recursive: root.recursive,
                });
            }
            None => {
                changes.push(CollectionWatchPathChange::Unwatch(path.clone()));
            }
        }
    }
    for (path, root) in &next_plan.roots {
        if !watched_paths.contains_key(path) {
            changes.push(CollectionWatchPathChange::Watch {
                path: path.clone(),
                recursive: root.recursive,
            });
        }
    }
    changes
}

async fn build_collection_watch_plan(state: &SharedState) -> Result<CollectionWatchPlan> {
    let snapshot = runtime_config_snapshot(state)?;
    let watcher_config = snapshot.config.collection_watcher;
    let ignored_collections = watcher_config
        .ignore_collections
        .iter()
        .map(|name| name.trim().to_string())
        .filter(|name| !name.is_empty())
        .collect::<BTreeSet<_>>();
    let state_for_status = Arc::clone(state);
    let plan = with_task_store_read(state, move |store| {
        let collections = store.list_collections()?;
        let mut plan = CollectionWatchPlan::default();
        for collection in collections {
            let ignored_by_config =
                !watcher_config.enabled || ignored_collections.contains(&collection.name);
            let mut watched_root_count = 0_usize;
            if collection.watch_enabled && !ignored_by_config {
                let roots = store.list_collection_roots(&collection.name)?;
                for root in roots {
                    for path in
                        watch_paths_for_collection_root(&root.path, root.canonical_path.as_ref())
                    {
                        if collection_watcher_path_ignored(&path, &watcher_config.ignore_paths) {
                            continue;
                        }
                        watched_root_count += 1;
                        let recursive = if path.is_dir() {
                            RecursiveMode::Recursive
                        } else {
                            RecursiveMode::NonRecursive
                        };
                        plan.roots
                            .entry(path)
                            .and_modify(|root: &mut CollectionWatchRoot| {
                                root.collections.insert(collection.name.clone());
                                if recursive == RecursiveMode::Recursive {
                                    root.recursive = RecursiveMode::Recursive;
                                }
                            })
                            .or_insert_with(|| CollectionWatchRoot {
                                recursive,
                                collections: BTreeSet::from([collection.name.clone()]),
                            });
                    }
                }
            }
            update_collection_watcher_status(&state_for_status, &collection.name, |status| {
                status.active =
                    collection.watch_enabled && !ignored_by_config && watched_root_count > 0;
                status.ignored_by_config = ignored_by_config;
                status.watched_root_count = watched_root_count;
            });
        }
        Ok::<_, anyhow::Error>(plan)
    })
    .await?;
    Ok(plan)
}

fn watch_paths_for_collection_root(
    path: &FsPath,
    canonical_path: Option<&PathBuf>,
) -> Vec<PathBuf> {
    let mut paths = BTreeSet::new();
    paths.insert(path.to_path_buf());
    if let Some(canonical_path) = canonical_path {
        paths.insert(canonical_path.clone());
    }
    paths.into_iter().collect()
}

fn collection_watcher_path_ignored(path: &FsPath, patterns: &[String]) -> bool {
    if patterns.is_empty() {
        return false;
    }
    let normalized = path
        .to_string_lossy()
        .replace('\\', "/")
        .trim_start_matches('/')
        .to_string();
    let rules = CollectionIgnoreRules::new(patterns);
    rules.is_ignored(&normalized, path.is_dir()) || rules.is_ignored(&normalized, true)
}

fn collection_names_for_event_paths(plan: &CollectionWatchPlan, paths: &[PathBuf]) -> Vec<String> {
    let mut names = BTreeSet::new();
    for path in paths {
        for (root, watch_root) in &plan.roots {
            if path_starts_with(path, root) {
                names.extend(watch_root.collections.iter().cloned());
            }
        }
    }
    names.into_iter().collect()
}

fn path_starts_with(path: &FsPath, root: &FsPath) -> bool {
    path == root || path.starts_with(root)
}

fn mark_active_collection_watchers_error(state: &SharedState, error: &str) {
    match state.collection_watcher.statuses.lock() {
        Ok(mut statuses) => {
            let error = bounded_collection_watcher_error(error);
            for status in statuses.values_mut().filter(|status| status.active) {
                status.last_error = Some(error.clone());
            }
        }
        Err(lock_error) => {
            tracing::warn!(error = %lock_error, "failed to record collection watcher notify error");
        }
    }
}

async fn maintain_collection_after_watch_event(
    state: &SharedState,
    collection_name: &str,
) -> Result<CollectionMaintenanceOutcome> {
    let snapshot = runtime_config_snapshot(state)?;
    let watcher_config = snapshot.config.collection_watcher;
    let watcher_config_for_sync = watcher_config.clone();
    let collection_name = collection_name.to_string();
    let state_for_sync = Arc::clone(state);
    let collection_name_for_sync = collection_name.clone();
    let sync_outcome = tokio::task::spawn_blocking(move || {
        let runtime = tokio::runtime::Handle::current();
        run_with_pipeline(state_for_sync, move |pipeline| {
            let collection = pipeline
                .store()
                .get_collection(&collection_name_for_sync)?
                .with_context(|| format!("collection not found: {collection_name_for_sync}"))?;
            if !collection.watch_enabled || !watcher_config_for_sync.enabled {
                return Ok::<_, anyhow::Error>(CollectionMaintenanceSyncOutcome::default());
            }
            if watcher_config_for_sync
                .ignore_collections
                .iter()
                .any(|name| name == &collection.name)
            {
                return Ok(CollectionMaintenanceSyncOutcome::default());
            }
            let old_members = pipeline
                .store()
                .list_collection_members(&collection_name_for_sync)?;
            let report = pipeline.sync_collection_with_extra_ignores(
                &collection_name_for_sync,
                &[],
                Some(watcher_config_for_sync.max_depth.max(1)),
                &watcher_config_for_sync.ignore_paths,
            )?;
            let new_members = pipeline
                .store()
                .list_collection_members(&collection_name_for_sync)?;
            let new_candidates = new_members
                .iter()
                .map(|member| CollectionMemberCandidate {
                    source_id: member.source_id.clone(),
                    logical_path: member.logical_path.clone(),
                    source_path: member.source_path.clone(),
                })
                .collect::<Vec<_>>();
            let diff = diff_collection_members(&old_members, &new_candidates);
            let stale = pipeline.check_stale()?;
            let member_source_ids = new_members
                .iter()
                .map(|member| member.source_id.clone())
                .collect::<HashSet<_>>();
            let mut ingest_candidates = BTreeMap::new();
            for candidate in &diff.added {
                match pipeline.source_ingest_freshness(&candidate.source_id)? {
                    SourceIngestFreshness::NeedsIngest(reason) => {
                        let candidate = collection_maintenance_ingest_candidate(
                            pipeline,
                            candidate.source_id.clone(),
                            reason.as_str(),
                        )?;
                        ingest_candidates
                            .entry(candidate.source_id.clone())
                            .or_insert(candidate);
                    }
                    SourceIngestFreshness::Fresh => {
                        tracing::info!(
                            collection = %collection_name_for_sync,
                            source = %candidate.source_id.0,
                            reason = SourceIngestFreshness::Fresh.as_str(),
                            "collection watcher skipped unchanged source ingest"
                        );
                    }
                    SourceIngestFreshness::Missing => {
                        tracing::info!(
                            collection = %collection_name_for_sync,
                            source = %candidate.source_id.0,
                            reason = SourceIngestFreshness::Missing.as_str(),
                            "collection watcher skipped missing source ingest"
                        );
                    }
                }
            }
            for source_id in stale
                .into_iter()
                .filter(|source_id| member_source_ids.contains(source_id))
            {
                let candidate =
                    collection_maintenance_ingest_candidate(pipeline, source_id, "stale")?;
                ingest_candidates
                    .entry(candidate.source_id.clone())
                    .or_insert(candidate);
            }
            for removed in &diff.removed {
                if !removed.source_path.exists()
                    && pipeline.store().get_source(&removed.source_id)?.is_some()
                {
                    runtime
                        .block_on(pipeline.remove_source_for_housekeeping(&removed.source_id))?;
                    ingest_candidates.remove(&removed.source_id);
                }
            }
            let skipped_unchanged_ingest = new_members
                .iter()
                .filter(|member| !ingest_candidates.contains_key(&member.source_id))
                .filter_map(|member| pipeline.source_ingest_freshness(&member.source_id).ok())
                .filter(|freshness| *freshness == SourceIngestFreshness::Fresh)
                .count();
            Ok(CollectionMaintenanceSyncOutcome {
                added: report.added,
                removed: report.removed,
                unchanged: report.unchanged,
                auto_index_enabled: collection.auto_index_enabled,
                skipped_unchanged_ingest,
                ingest_candidates: ingest_candidates.into_values().collect(),
            })
        })
    })
    .await
    .context("join collection watcher sync task")??;

    let mut outcome = CollectionMaintenanceOutcome {
        added: sync_outcome.added,
        removed: sync_outcome.removed,
        unchanged: sync_outcome.unchanged,
        queued_task_ids: Vec::new(),
    };
    if !sync_outcome.auto_index_enabled {
        return Ok(outcome);
    }
    if sync_outcome.skipped_unchanged_ingest > 0 {
        tracing::info!(
            collection = %collection_name,
            skipped_unchanged = sync_outcome.skipped_unchanged_ingest,
            "collection watcher skipped unchanged sources during auto-index maintenance"
        );
    }
    for candidate in sync_outcome
        .ingest_candidates
        .into_iter()
        .take(watcher_config.max_queued_tasks.max(1))
    {
        if let Some(task_id) = create_collection_watcher_ingest_task_if_no_active_intent(
            state,
            &collection_name,
            candidate,
        )
        .await?
        {
            outcome.queued_task_ids.push(task_id.0);
        }
    }
    if !outcome.queued_task_ids.is_empty() {
        schedule_ingest_queue(Arc::clone(state));
    }
    Ok(outcome)
}

#[derive(Default)]
struct CollectionMaintenanceSyncOutcome {
    added: usize,
    removed: usize,
    unchanged: usize,
    auto_index_enabled: bool,
    skipped_unchanged_ingest: usize,
    ingest_candidates: Vec<CollectionMaintenanceIngestCandidate>,
}

struct CollectionMaintenanceIngestCandidate {
    source_id: SourceId,
    reason: &'static str,
    source_hash: Option<String>,
}

fn collection_maintenance_ingest_candidate(
    pipeline: &IngestPipeline,
    source_id: SourceId,
    reason: &'static str,
) -> Result<CollectionMaintenanceIngestCandidate> {
    let source_hash = pipeline.source_ingest_snapshot(&source_id)?.current_hash;
    Ok(CollectionMaintenanceIngestCandidate {
        source_id,
        reason,
        source_hash,
    })
}

fn collection_watcher_ingest_request_metadata(
    collection_name: &str,
    candidate: &CollectionMaintenanceIngestCandidate,
) -> serde_json::Value {
    let mut request = ingest_task_request_metadata_with_source_hash(
        ingest_task_request_metadata_with_queue_claim(
            Some(&candidate.source_id.0),
            false,
            None,
            false,
            true,
        ),
        candidate.source_hash.as_deref(),
    );
    if let serde_json::Value::Object(map) = &mut request {
        map.insert(
            "collection_name".into(),
            serde_json::Value::String(collection_name.to_string()),
        );
        map.insert(
            "enqueue_reason".into(),
            serde_json::Value::String(candidate.reason.to_string()),
        );
    }
    bounded_json(request)
}

async fn create_collection_watcher_ingest_task_if_no_active_intent(
    state: &SharedState,
    collection_name: &str,
    candidate: CollectionMaintenanceIngestCandidate,
) -> Result<Option<TaskId>> {
    let collection_name = collection_name.to_string();
    with_task_store_write(state, move |store| {
        if store_has_active_ingest_task(
            store,
            &candidate.source_id,
            candidate.source_hash.as_deref(),
        )? {
            return Ok(None);
        }
        let task_id = TaskId::new();
        let task = store.create_task(
            &task_id,
            TaskKind::Ingest,
            &collection_watcher_ingest_request_metadata(&collection_name, &candidate),
        )?;
        let payload = queued_event_payload(store, task)?;
        store.insert_task_event(&task_id, "queued", "task queued", &payload)?;
        Ok(Some(task_id))
    })
    .await
}

fn store_has_active_ingest_task(
    store: &Store,
    source_id: &SourceId,
    source_hash: Option<&str>,
) -> Result<bool> {
    for task in store.active_tasks(TaskKind::Ingest)? {
        let request: PersistedIngestRequest = match serde_json::from_value(task.request) {
            Ok(request) => request,
            Err(_) => continue,
        };
        if request.operation.as_deref().unwrap_or("ingest") != "ingest"
            || request.source_id.as_deref() != Some(source_id.0.as_str())
        {
            continue;
        }

        if request.vectors_only {
            continue;
        }
        if active_ingest_matches_source_hash(source_hash, request.source_hash.as_deref()) {
            return Ok(true);
        }
    }
    Ok(false)
}

fn active_ingest_matches_source_hash(
    candidate_source_hash: Option<&str>,
    active_source_hash: Option<&str>,
) -> bool {
    match (candidate_source_hash, active_source_hash) {
        (Some(candidate_hash), Some(active_hash)) => candidate_hash == active_hash,
        (None, None) => true,
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Error helper
// ---------------------------------------------------------------------------

fn err(status: StatusCode, e: anyhow::Error) -> (StatusCode, Json<ErrorResponse>) {
    let upstream_failure = upstream_failure_value(&e);
    if let Some(upstream_failure) = &upstream_failure {
        tracing::warn!(
            upstream_failure = %upstream_failure,
            "upstream request failure diagnostic recorded"
        );
    }
    (
        status,
        Json({
            let mut response = ErrorResponse::new(format!("{e:#}"));
            response.upstream_failure = upstream_failure.map(Box::new);
            response
        }),
    )
}

fn upstream_failure_value(error: &anyhow::Error) -> Option<serde_json::Value> {
    let diagnostic = error.chain().find_map(|cause| {
        if let Some(provider_error) = cause.downcast_ref::<ProviderError>() {
            return provider_error.diagnostic();
        }
        if let Some(upstream_error) = cause.downcast_ref::<UpstreamFailureError>() {
            return Some(upstream_error.diagnostic());
        }
        None
    })?;
    serde_json::to_value(diagnostic).ok()
}

// ---------------------------------------------------------------------------
// Config reload
// ---------------------------------------------------------------------------

fn record_config_reload_error(state: &SharedState, error: &str) -> Result<ConfigReloadMetadata> {
    let mut runtime = state
        .runtime_config
        .write()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    runtime.reload.last_reload_at = Some(unix_timestamp_string());
    runtime.reload.last_reload_error = Some(safe_config_reload_error(error));
    runtime.reload.last_applied_reload_safe_keys.clear();
    runtime.reload.last_restart_required_keys.clear();
    Ok(runtime.reload.clone())
}

fn config_reload_rejection(state: &SharedState, error: anyhow::Error) -> anyhow::Error {
    let message = safe_config_reload_error(&format!("{error:#}"));
    let _ = record_config_reload_error(state, &message);
    anyhow::anyhow!("config reload rejected: {message}")
}

async fn reload_config_from_path(state: &SharedState) -> Result<ConfigReloadMetadata> {
    let config_path = state.config_path.clone();
    let candidate =
        Config::load_from(&config_path).map_err(|error| config_reload_rejection(state, error))?;

    let current = runtime_config_snapshot(state)
        .map_err(|error| config_reload_rejection(state, error))?
        .config;
    let plan = current
        .reload_plan(&candidate)
        .map_err(|error| config_reload_rejection(state, error))?;
    let collection_watcher_plan_changed =
        collection_watcher_plan_reload_keys_changed(&plan.reload_safe_keys);
    let next_config = current.apply_reload_safe_changes(&candidate);
    let next_config_for_pipeline = next_config.clone();
    let state_for_pipeline = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let mut pipeline = state_for_pipeline
            .pipeline
            .lock()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        let pipeline = pipeline_mut(&mut pipeline)?;
        pipeline.reload_runtime_config(&next_config_for_pipeline)
    })
    .await
    .context("join config reload task")
    .and_then(|result| result)
    .map_err(|error| config_reload_rejection(state, error))?;

    let restart_required_keys = plan.restart_required_keys;
    let reload_error = restart_required_message(&restart_required_keys);
    let metadata = ConfigReloadMetadata {
        active_config_path: config_path.display().to_string(),
        loaded_at: runtime_config_snapshot(state)
            .map_err(|error| config_reload_rejection(state, error))?
            .reload
            .loaded_at,
        last_reload_at: Some(unix_timestamp_string()),
        last_reload_error: reload_error,
        last_applied_reload_safe_keys: plan.reload_safe_keys,
        last_restart_required_keys: restart_required_keys,
    };

    {
        let mut runtime = state
            .runtime_config
            .write()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        runtime.config = next_config;
        runtime.reload = metadata.clone();
    }
    configure_daemon_resources(
        &state.resources,
        &runtime_config_snapshot(state)?.config.daemon.resources,
    );
    state
        .memory_budget
        .configure_from(&runtime_config_snapshot(state)?.config.daemon.resources)?;
    if collection_watcher_plan_changed {
        send_collection_watcher_command(state, CollectionWatcherCommand::Refresh);
    }
    queue_idle_exit_collection_watcher_resync_if_enabled(state);
    Ok(metadata)
}

fn collection_watcher_plan_reload_keys_changed(keys: &[String]) -> bool {
    keys.iter().any(|key| {
        matches!(
            key.as_str(),
            "collection_watcher.enabled"
                | "collection_watcher.ignore_collections"
                | "collection_watcher.ignore_paths"
        )
    })
}

fn restart_required_message(keys: &[ConfigRestartRequiredKey]) -> Option<String> {
    if keys.is_empty() {
        return None;
    }
    let key_list = keys
        .iter()
        .map(|change| change.key.as_str())
        .collect::<Vec<_>>()
        .join(", ");
    Some(safe_config_reload_error(&format!(
        "restart or reindex required for config key(s): {key_list}; reload-safe keys were applied when present"
    )))
}

fn start_config_watcher(state: SharedState) -> Result<RecommendedWatcher> {
    let config_path = state.config_path.clone();
    let watch_dir = config_path
        .parent()
        .map(FsPath::to_path_buf)
        .unwrap_or_else(|| PathBuf::from("."));
    let (tx, mut rx) = mpsc::unbounded_channel();
    let mut watcher = notify::recommended_watcher(move |event| {
        let _ = tx.send(event);
    })
    .context("create config watcher")?;
    watcher
        .watch(&watch_dir, RecursiveMode::NonRecursive)
        .with_context(|| format!("watch config directory: {}", watch_dir.display()))?;

    tokio::spawn(async move {
        while let Some(event) = rx.recv().await {
            if !config_watch_event_matches(&event, &config_path, &state) {
                continue;
            }
            tokio::time::sleep(CONFIG_RELOAD_DEBOUNCE).await;
            while let Ok(event) = rx.try_recv() {
                let _ = config_watch_event_matches(&event, &config_path, &state);
            }

            match reload_config_from_path(&state).await {
                Ok(metadata) => {
                    if metadata.last_reload_error.is_some() {
                        tracing::warn!(
                            reload_safe_keys = ?metadata.last_applied_reload_safe_keys,
                            restart_required_keys = ?metadata
                                .last_restart_required_keys
                                .iter()
                                .map(|change| change.key.as_str())
                                .collect::<Vec<_>>(),
                            "config reload partially applied; restart or reindex required for some keys"
                        );
                    } else {
                        tracing::info!(
                            reload_safe_keys = ?metadata.last_applied_reload_safe_keys,
                            "config reload applied"
                        );
                    }
                }
                Err(error) => {
                    tracing::warn!(
                        error = %safe_config_reload_error(&error.to_string()),
                        "config reload rejected"
                    );
                }
            }
        }
    });

    Ok(watcher)
}

fn config_watch_event_matches(
    event: &notify::Result<notify::Event>,
    config_path: &FsPath,
    state: &SharedState,
) -> bool {
    match event {
        Ok(event) => {
            if !matches!(
                event.kind,
                EventKind::Any | EventKind::Create(_) | EventKind::Modify(_) | EventKind::Remove(_)
            ) {
                return false;
            }
            let config_dir = config_path.parent();
            event.paths.is_empty()
                || event.paths.iter().any(|path| {
                    path == config_path
                        || config_dir.is_some_and(|config_dir| path.parent() == Some(config_dir))
                })
        }
        Err(error) => {
            let message = safe_config_reload_error(&format!("config watch error: {error}"));
            let _ = record_config_reload_error(state, &message);
            tracing::warn!(error = %message, "config watch error");
            false
        }
    }
}

// ---------------------------------------------------------------------------
// Shutdown signal
// ---------------------------------------------------------------------------

async fn shutdown_signal(mut idle_exit: watch::Receiver<bool>) {
    let ctrl_c = async {
        signal::ctrl_c()
            .await
            .expect("failed to install ctrl+c handler");
    };

    #[cfg(unix)]
    let terminate = async {
        signal::unix::signal(signal::unix::SignalKind::terminate())
            .expect("failed to install SIGTERM handler")
            .recv()
            .await;
    };

    #[cfg(not(unix))]
    let terminate = std::future::pending::<()>();

    let idle_exit_signal = async move {
        loop {
            if *idle_exit.borrow() {
                break;
            }
            if idle_exit.changed().await.is_err() {
                std::future::pending::<()>().await;
            }
        }
    };

    tokio::select! {
        () = ctrl_c => {
            tracing::info!("shutdown signal received");
        },
        () = terminate => {
            tracing::info!("shutdown signal received");
        },
        () = idle_exit_signal => {
            tracing::info!("idle exit shutdown signal received");
        },
    }
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

fn main() -> Result<()> {
    if write_version_if_requested(std::env::args().skip(1), &mut std::io::stdout())? {
        return Ok(());
    }

    // Read worker_threads from config before creating the runtime.
    let config_path = config::config_path();
    let config = Config::load_from(&config_path).context("failed to load config")?;
    auth_middleware::validate_daemon_auth_bind(&config.daemon)?;
    let worker_threads = if config.daemon.worker_threads == 0 {
        std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(4)
    } else {
        config.daemon.worker_threads
    };

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(worker_threads)
        .enable_all()
        .build()
        .context("failed to create tokio runtime")?;

    block_on_daemon_with_shutdown_timeout(
        runtime,
        run_daemon_with_config(config),
        DAEMON_RUNTIME_SHUTDOWN_TIMEOUT,
    )
}

fn block_on_daemon_with_shutdown_timeout<F, T>(
    runtime: tokio::runtime::Runtime,
    future: F,
    shutdown_timeout: Duration,
) -> T
where
    F: Future<Output = T>,
{
    let result = runtime.block_on(future);
    runtime.shutdown_timeout(shutdown_timeout);
    result
}

fn write_version_if_requested<I, W>(args: I, stdout: &mut W) -> Result<bool>
where
    I: IntoIterator,
    I::Item: AsRef<str>,
    W: Write,
{
    match args.into_iter().next() {
        Some(arg) if matches!(arg.as_ref(), "-V" | "--version") => {
            writeln!(stdout, "verbatim-daemon {}", env!("CARGO_PKG_VERSION"))?;
            Ok(true)
        }
        _ => Ok(false),
    }
}

fn daemon_router(state: SharedState) -> Router {
    routes::build_router(state)
}

struct StartupRuntimeHandles {
    _config_watcher: RecommendedWatcher,
    _collection_watcher: tokio::task::JoinHandle<()>,
    _idle_reclaim_scheduler: tokio::task::JoinHandle<()>,
    _deletion_reconcile_scheduler: tokio::task::JoinHandle<()>,
    _idle_exit_scheduler: tokio::task::JoinHandle<()>,
}

struct StartupPipelineInit {
    pipeline: IngestPipeline,
    fts_startup_maintenance: FtsMaintenanceOutcome,
    index_status_cache: Option<IndexStatusResponse>,
    index_gc_error: Option<String>,
}

enum DaemonStartupRace<T> {
    StartupFinished(T),
    ServerExited,
}

async fn await_startup_or_server_exit<T, F>(
    startup: F,
    server_task: &mut tokio::task::JoinHandle<Result<()>>,
) -> Result<DaemonStartupRace<T>>
where
    F: std::future::Future<Output = T>,
{
    tokio::pin!(startup);
    tokio::select! {
        startup_result = &mut startup => Ok(DaemonStartupRace::StartupFinished(startup_result)),
        server_result = server_task => {
            server_result
                .context("join daemon HTTP server task")??;
            Ok(DaemonStartupRace::ServerExited)
        }
    }
}

async fn run_daemon_with_config(config: Config) -> Result<()> {
    auth_middleware::validate_daemon_auth_bind(&config.daemon)?;
    tracing_subscriber::fmt::init();

    let config_path = config::config_path();
    let data_dir = config::data_dir(&config);
    fs::create_dir_all(&data_dir)
        .with_context(|| format!("create data dir: {}", data_dir.display()))?;
    let memory_budget = MemoryBudget::from_config(&config.daemon.resources);
    let task_store = sqlite_durability_ops::open_task_store(&config, &data_dir)?;

    let bind_addr = config.daemon.bind.clone();

    let state: SharedState = Arc::new(AppState {
        pipeline: std::sync::Mutex::new(None),
        task_store: std::sync::Mutex::new(task_store),
        index_status_cache: std::sync::RwLock::new(None),
        readiness: std::sync::RwLock::new(ReadinessHealth::starting(
            "initializing_pipeline",
            Some("initializing indexes and startup maintenance".into()),
        )),
        resources: daemon_resources(&config.daemon.resources),
        memory_budget,
        ingest_queue_active: AtomicBool::new(false),
        #[cfg(test)]
        ingest_queue_drain_receipt: std::sync::Mutex::new(None),
        ingest_worker_active: AtomicBool::new(false),
        collection_watcher: CollectionWatcherRuntime::default(),
        idle_reclaim: Arc::new(IdleReclaimRuntime::new(now_unix_millis())),
        idle_exit: Arc::new(IdleExitRuntime::new(now_unix_millis())),
        #[cfg(test)]
        idle_reclaim_before_backend_hook: std::sync::Mutex::new(None),
        #[cfg(test)]
        idle_reclaim_before_backend_call_hook: std::sync::Mutex::new(None),
        runtime_config: std::sync::RwLock::new(RuntimeConfigState {
            config: config.clone(),
            reload: initial_reload_metadata(&config_path),
        }),
        config_path,
        data_dir: data_dir.clone(),
    });
    let _memory_budget_sampler = state.memory_budget.start_memory_sampler();
    let (idle_exit_shutdown_tx, idle_exit_shutdown_rx) = watch::channel(false);
    let app = daemon_router(Arc::clone(&state));

    tracing::info!(bind = %bind_addr, "starting verbatim daemon");

    let listener = TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind {bind_addr}"))?;

    let mut server_task = tokio::spawn(async move {
        axum::serve(
            listener,
            app.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .with_graceful_shutdown(shutdown_signal(idle_exit_shutdown_rx))
        .await
        .context("server error")
    });

    let startup =
        finish_daemon_startup(Arc::clone(&state), config, data_dir, idle_exit_shutdown_tx);
    let _startup_handles = match await_startup_or_server_exit(startup, &mut server_task).await? {
        DaemonStartupRace::ServerExited => return Ok(()),
        DaemonStartupRace::StartupFinished(Ok(handles)) => Some(handles),
        DaemonStartupRace::StartupFinished(Err(error)) => {
            let reason = error.to_string();
            tracing::error!(error = %reason, "daemon startup maintenance failed");
            set_readiness(
                &state,
                ReadinessHealth::degraded("startup_failed", Some(reason)),
            );
            None
        }
    };

    server_task
        .await
        .context("join daemon HTTP server task")??;

    sqlite_durability_ops::shutdown_checkpoint(&state);

    Ok(())
}

async fn finish_daemon_startup(
    state: SharedState,
    config: Config,
    data_dir: PathBuf,
    idle_exit_shutdown_tx: watch::Sender<bool>,
) -> Result<StartupRuntimeHandles> {
    set_readiness(
        &state,
        ReadinessHealth::starting(
            "initializing_pipeline",
            Some("initializing indexes and startup maintenance".into()),
        ),
    );
    let startup = start_startup_pipeline_init(config.clone(), data_dir.clone())
        .await
        .context("join startup pipeline initialization")??;
    log_fts_startup_maintenance(startup.fts_startup_maintenance);
    if let Some(error) = startup.index_gc_error {
        tracing::warn!(error = %error, "startup index generation garbage collection failed");
    }
    {
        let mut cache = state
            .index_status_cache
            .write()
            .map_err(|error| anyhow::anyhow!("{error}"))?;
        *cache = startup.index_status_cache;
    }
    restore_pipeline(&state, startup.pipeline)?;
    reconcile_deletions_on_startup(&state, STARTUP_DELETION_RECONCILE_BATCH_SIZE)
        .await
        .context("reconcile persisted source deletions")?;

    set_readiness(
        &state,
        ReadinessHealth::starting(
            "orphan_recovery",
            Some("recovering previous running ingest tasks".into()),
        ),
    );
    // Fail orphaned ingest tasks left in `running` status by a previous daemon
    // session that was killed before completing them.  Without this, the stale
    // running task blocks the entire ingest queue because every queue-drain path
    // refuses to start new work while a running task exists (#182).
    match recover_orphaned_running_tasks(&state).await {
        Ok(0) => {}
        Ok(recovered) => {
            tracing::warn!(
                recovered,
                "failed orphaned running ingest tasks from previous daemon session"
            );
        }
        Err(error) => {
            tracing::error!(error = %error, "failed to recover orphaned running ingest tasks");
        }
    }

    set_readiness(
        &state,
        ReadinessHealth::starting(
            "watcher_startup",
            Some("starting config, collection, and background schedulers".into()),
        ),
    );
    let config_watcher = start_config_watcher(Arc::clone(&state))?;
    let collection_watcher = start_collection_watcher(Arc::clone(&state))?;
    queue_idle_exit_collection_watcher_resync_if_enabled(&state);
    let idle_reclaim_scheduler = start_idle_reclaim_scheduler(Arc::clone(&state));
    let idle_exit_scheduler = start_idle_exit_scheduler(Arc::clone(&state), idle_exit_shutdown_tx);
    schedule_ingest_queue(Arc::clone(&state));
    set_readiness(&state, ReadinessHealth::ready());
    let deletion_reconcile_scheduler = start_deletion_reconcile_scheduler(Arc::clone(&state));

    Ok(StartupRuntimeHandles {
        _config_watcher: config_watcher,
        _collection_watcher: collection_watcher,
        _idle_reclaim_scheduler: idle_reclaim_scheduler,
        _deletion_reconcile_scheduler: deletion_reconcile_scheduler,
        _idle_exit_scheduler: idle_exit_scheduler,
    })
}

async fn start_startup_pipeline_init(
    config: Config,
    data_dir: PathBuf,
) -> Result<Result<StartupPipelineInit>> {
    let (tx, rx) = tokio::sync::oneshot::channel();
    std::thread::Builder::new()
        .name("verbatim-startup-init".into())
        .spawn(move || {
            let _ = tx.send(startup_pipeline_init(config, data_dir));
        })
        .context("spawn startup pipeline initialization thread")?;
    rx.await
        .context("startup pipeline initialization thread exited before reporting")
}

fn startup_pipeline_init(config: Config, data_dir: PathBuf) -> Result<StartupPipelineInit> {
    let pipeline =
        IngestPipeline::new(&config, &data_dir).context("failed to initialize ingest pipeline")?;
    let fts_startup_maintenance = pipeline.fts_startup_maintenance();
    let index_gc_error = apply_index_gc(&data_dir, pipeline.store(), config.index_gc.policy())
        .err()
        .map(|error| error.to_string());
    let index_status_cache = initial_index_status_cache(&pipeline);

    Ok(StartupPipelineInit {
        pipeline,
        fts_startup_maintenance,
        index_status_cache,
        index_gc_error,
    })
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct FtsStartupMaintenanceLogFields {
    status: &'static str,
    reason: &'static str,
    child_rows: u64,
    fts_rows: u64,
    missing_rows: u64,
    orphan_rows: u64,
    duration_ms: u64,
}

fn fts_startup_maintenance_log_fields(
    outcome: FtsMaintenanceOutcome,
) -> FtsStartupMaintenanceLogFields {
    FtsStartupMaintenanceLogFields {
        status: outcome.status.as_str(),
        reason: outcome.reason.as_str(),
        child_rows: outcome.counts.child_rows,
        fts_rows: outcome.counts.fts_rows,
        missing_rows: outcome.counts.missing_rows,
        orphan_rows: outcome.counts.orphan_rows,
        duration_ms: duration_millis_u64(outcome.duration),
    }
}

fn duration_millis_u64(duration: Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

fn log_fts_startup_maintenance(outcome: FtsMaintenanceOutcome) {
    let fields = fts_startup_maintenance_log_fields(outcome);
    tracing::info!(
        status = fields.status,
        reason = fields.reason,
        child_rows = fields.child_rows,
        fts_rows = fields.fts_rows,
        missing_rows = fields.missing_rows,
        orphan_rows = fields.orphan_rows,
        duration_ms = fields.duration_ms,
        "SQLite FTS startup maintenance complete"
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use verbatim_core::index::sqlite_fts::{
        FtsMaintenanceCounts, FtsMaintenanceReason, FtsMaintenanceStatus,
    };
    use verbatim_core::retrieve::refresh_final_evidence_pack_debug;
    use verbatim_core::types::{
        CanonicalLocator, Chunk, ChunkId, ChunkType, EmbeddingCacheStats, EvidenceKind,
        EvidenceUnit, GraphNode, GraphNodeId, GraphNodeKind, ReferenceComponent,
        RetrievalDebugEvidencePackMode, RetrievalDenseVectorPath, RetrievalEvidencePackEntry,
        RetrievalEvidenceRole, RetrievalProvenance, RetrievalRerankStatus, Source, SourceLocator,
        VectorIndexResidency,
    };

    #[path = "ask_stream_verification_tests.rs"]
    mod ask_stream_verification_tests;
    #[path = "../auth_middleware_daemon_tests.rs"]
    mod auth_middleware_daemon_tests;
    #[path = "issue_332_explicit_move_route_tests.rs"]
    mod issue_332_explicit_move_route_tests;
    #[path = "source_bounded_output_tests.rs"]
    mod source_bounded_output_tests;
    #[path = "sql_statement_telemetry_tests.rs"]
    mod sql_statement_telemetry_tests;
    #[path = "sqlite_durability_tests.rs"]
    mod sqlite_durability_tests;

    use source_bounded_output_tests::persisted_retrieve_response;

    fn has_task_terminalize_span(spans: &[verbatim_core::task::TaskSpan]) -> bool {
        spans
            .iter()
            .any(|span| span.phase == IngestTaskStage::TaskTerminalize.as_str())
    }

    fn minimal_task_profile(task_id: &TaskId, kind: TaskKind, status: TaskStatus) -> TaskProfile {
        TaskProfile {
            schema_version: verbatim_core::task::TASK_PROFILE_SCHEMA_VERSION,
            task_id: task_id.clone(),
            task_kind: kind,
            status,
            queue_wait_ms: 0,
            total_wall_ms: 1,
            controls: Default::default(),
            resources: Default::default(),
            endpoints: Vec::new(),
            retrieve: None,
            ask: None,
        }
    }

    #[test]
    fn version_flag_prints_package_version() {
        let mut stdout = Vec::new();

        let handled = write_version_if_requested(["--version"], &mut stdout).unwrap();

        assert!(handled);
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            format!("verbatim-daemon {}\n", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn short_version_flag_prints_package_version() {
        let mut stdout = Vec::new();

        let handled = write_version_if_requested(["-V"], &mut stdout).unwrap();

        assert!(handled);
        assert_eq!(
            String::from_utf8(stdout).unwrap(),
            format!("verbatim-daemon {}\n", env!("CARGO_PKG_VERSION"))
        );
    }

    #[test]
    fn ocr_evidence_kind_and_retrieval_role_names_are_distinct() {
        assert_eq!(evidence_kind_name(EvidenceKind::Ocr), "ocr");
        assert_eq!(
            retrieval_role_name(RetrievalEvidenceRole::OcrText),
            "ocr_text"
        );
    }

    #[test]
    fn no_args_continue_to_daemon_startup() {
        let mut stdout = Vec::new();

        let handled = write_version_if_requested(std::iter::empty::<&str>(), &mut stdout).unwrap();

        assert!(!handled);
        assert!(stdout.is_empty());
    }

    #[test]
    fn runtime_shutdown_timeout_returns_while_blocking_task_is_still_running() {
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let finished = Arc::new(AtomicBool::new(false));
        let finished_for_task = Arc::clone(&finished);
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(1)
            .enable_all()
            .build()
            .unwrap();

        let start = Instant::now();
        let result = block_on_daemon_with_shutdown_timeout(
            runtime,
            async move {
                tokio::task::spawn_blocking(move || {
                    started_tx.send(()).unwrap();
                    let _ = release_rx.recv();
                    finished_for_task.store(true, Ordering::Release);
                });
                started_rx.recv_timeout(Duration::from_secs(1)).unwrap();
                Ok::<_, anyhow::Error>(())
            },
            Duration::from_millis(25),
        );

        assert!(result.is_ok());
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "runtime shutdown waited for a blocking task instead of honoring shutdown_timeout"
        );
        assert!(!finished.load(Ordering::Acquire));
        release_tx.send(()).unwrap();
        for _ in 0..50 {
            if finished.load(Ordering::Acquire) {
                return;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        panic!("blocking task did not finish after test released it");
    }

    #[test]
    fn fts_startup_observability_fields_include_rebuilt_skipped_and_repaired_outcomes() {
        let cases = [
            (
                FtsMaintenanceOutcome {
                    status: FtsMaintenanceStatus::Rebuilt,
                    reason: FtsMaintenanceReason::MissingProjectionVersion,
                    counts: FtsMaintenanceCounts {
                        child_rows: 3,
                        fts_rows: 3,
                        missing_rows: 0,
                        orphan_rows: 0,
                    },
                    duration: Duration::from_millis(17),
                },
                ("rebuilt", "missing_projection_version", 3, 3, 0, 0, 17),
            ),
            (
                FtsMaintenanceOutcome {
                    status: FtsMaintenanceStatus::Skipped,
                    reason: FtsMaintenanceReason::Current,
                    counts: FtsMaintenanceCounts {
                        child_rows: 3,
                        fts_rows: 3,
                        missing_rows: 0,
                        orphan_rows: 0,
                    },
                    duration: Duration::from_millis(2),
                },
                ("skipped", "current", 3, 3, 0, 0, 2),
            ),
            (
                FtsMaintenanceOutcome {
                    status: FtsMaintenanceStatus::Repaired,
                    reason: FtsMaintenanceReason::OrphanRows,
                    counts: FtsMaintenanceCounts {
                        child_rows: 3,
                        fts_rows: 4,
                        missing_rows: 0,
                        orphan_rows: 1,
                    },
                    duration: Duration::from_millis(23),
                },
                ("repaired", "orphan_rows", 3, 4, 0, 1, 23),
            ),
        ];

        for (
            outcome,
            (
                expected_status,
                expected_reason,
                expected_child_rows,
                expected_fts_rows,
                expected_missing_rows,
                expected_orphan_rows,
                expected_duration_ms,
            ),
        ) in cases
        {
            let fields = fts_startup_maintenance_log_fields(outcome);

            assert_eq!(fields.status, expected_status);
            assert_eq!(fields.reason, expected_reason);
            assert_eq!(fields.child_rows, expected_child_rows);
            assert_eq!(fields.fts_rows, expected_fts_rows);
            assert_eq!(fields.missing_rows, expected_missing_rows);
            assert_eq!(fields.orphan_rows, expected_orphan_rows);
            assert_eq!(fields.duration_ms, expected_duration_ms);
        }
    }

    #[test]
    fn sqlite_writer_resource_limits_keep_single_active_writer() {
        let config = DaemonResourceConfig {
            sqlite_writer_concurrency: 32,
            sqlite_writer_queue_capacity: 7,
            sqlite_writer_queue_timeout_seconds: 11,
            ..DaemonResourceConfig::default()
        };

        let limits = sqlite_writer_resource_limits(&config.bounded());

        assert_eq!(limits.capacity, SQLITE_WRITER_ACTIVE_CAPACITY);
        assert_eq!(limits.queue_capacity, 7);
        assert_eq!(limits.queue_timeout, Duration::from_secs(11));
    }

    #[tokio::test]
    async fn config_reload_applies_safe_changes_and_reports_metadata() {
        let test_dir = TestDir::new("config-reload-safe");
        let config_path = test_dir.path().join("config.toml");
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.retrieval.dense_top_k = 4;
        config.chat.timeout_seconds = 1;
        fs::write(&config_path, config.show().unwrap()).unwrap();
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state =
            test_state_with_config_path(config.clone(), test_dir.path(), pipeline, config_path);

        let mut candidate = config.clone();
        candidate.retrieval.dense_top_k = 9;
        candidate.chat.timeout_seconds = 8;
        fs::write(&state.config_path, candidate.show().unwrap()).unwrap();

        let metadata = reload_config_from_path(&state).await.unwrap();
        let snapshot = runtime_config_snapshot(&state).unwrap();
        let Json(response) = get_config(State(Arc::clone(&state))).await.unwrap();

        assert_eq!(snapshot.config.retrieval.dense_top_k, 9);
        assert_eq!(snapshot.config.chat.timeout_seconds, 8);
        assert_eq!(
            metadata.last_applied_reload_safe_keys,
            vec!["chat.timeout_seconds", "retrieval.dense_top_k"]
        );
        assert!(metadata.last_reload_error.is_none());
        assert_eq!(response.config["retrieval"]["dense_top_k"], 9);
        assert_eq!(
            response.reload.active_config_path,
            state.config_path.display().to_string()
        );
    }

    #[tokio::test]
    async fn config_reload_refreshes_collection_watcher_plan() {
        let test_dir = TestDir::new("config-reload-collection-watcher");
        let config_path = test_dir.path().join("config.toml");
        let root = test_dir.path().join("articles");
        fs::create_dir_all(&root).unwrap();
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.collection_watcher.enabled = false;
        fs::write(&config_path, config.show().unwrap()).unwrap();
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state =
            test_state_with_config_path(config.clone(), test_dir.path(), pipeline, config_path);
        let _ = create_collection(
            State(Arc::clone(&state)),
            Json(CreateCollectionRequest {
                name: "articles".into(),
                ignore_patterns: Vec::new(),
            }),
        )
        .await
        .unwrap();
        let _ = add_collection_root(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(AddCollectionRootRequest {
                path: root.display().to_string(),
            }),
        )
        .await
        .unwrap();
        let _ = update_collection_watcher(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionWatcherUpdateRequest {
                enabled: true,
                auto_index_enabled: None,
            }),
        )
        .await
        .unwrap();
        let _watcher = start_collection_watcher(Arc::clone(&state)).unwrap();

        let status = wait_for_collection_watcher_status(&state, "articles", |status| {
            status.ignored_by_config && !status.active
        })
        .await;
        assert_eq!(status.watched_root_count, 0);

        let mut candidate = config.clone();
        candidate.collection_watcher.enabled = true;
        fs::write(&state.config_path, candidate.show().unwrap()).unwrap();

        let metadata = reload_config_from_path(&state).await.unwrap();
        assert_eq!(
            metadata.last_applied_reload_safe_keys,
            vec!["collection_watcher.enabled"]
        );
        let status = wait_for_collection_watcher_status(&state, "articles", |status| {
            !status.ignored_by_config && status.active && status.watched_root_count > 0
        })
        .await;

        assert!(status.active);
        assert!(status.watched_root_count > 0);
    }

    #[tokio::test]
    async fn config_reload_rejects_invalid_config_and_keeps_previous_good_config() {
        const SECRET_SENTINEL: &str = "sk-verbatim-secret-reload-leak";

        let test_dir = TestDir::new("config-reload-invalid");
        let config_path = test_dir.path().join("config.toml");
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.retrieval.dense_top_k = 4;
        fs::write(&config_path, config.show().unwrap()).unwrap();
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state =
            test_state_with_config_path(config.clone(), test_dir.path(), pipeline, config_path);

        fs::write(
            &state.config_path,
            format!("[chat]\napi_key = \"{SECRET_SENTINEL}\n"),
        )
        .unwrap();

        let error = reload_config_from_path(&state).await.unwrap_err();
        let snapshot = runtime_config_snapshot(&state).unwrap();
        let metadata_error = snapshot
            .reload
            .last_reload_error
            .as_deref()
            .unwrap_or_default();
        let returned_error = error.to_string();

        assert!(returned_error.contains("config reload rejected"));
        assert!(!returned_error.contains(SECRET_SENTINEL));
        assert!(!returned_error.contains("api_key"));
        assert!(!metadata_error.contains(SECRET_SENTINEL));
        assert!(!metadata_error.contains("api_key"));
        assert!(!metadata_error.contains(" | "));
        assert_eq!(snapshot.config.retrieval.dense_top_k, 4);
        assert!(metadata_error.contains("failed to parse config TOML"));
        assert!(snapshot.reload.last_applied_reload_safe_keys.is_empty());
    }

    #[tokio::test]
    async fn config_reload_reports_restart_required_changes_without_applying_them() {
        let test_dir = TestDir::new("config-reload-restart-required");
        let config_path = test_dir.path().join("config.toml");
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.daemon.bind = "127.0.0.1:7700".into();
        config.retrieval.dense_top_k = 4;
        fs::write(&config_path, config.show().unwrap()).unwrap();
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state =
            test_state_with_config_path(config.clone(), test_dir.path(), pipeline, config_path);

        let mut candidate = config.clone();
        candidate.daemon.bind = "127.0.0.1:9900".into();
        candidate.retrieval.dense_top_k = 11;
        fs::write(&state.config_path, candidate.show().unwrap()).unwrap();

        let metadata = reload_config_from_path(&state).await.unwrap();
        let snapshot = runtime_config_snapshot(&state).unwrap();

        assert_eq!(snapshot.config.retrieval.dense_top_k, 11);
        assert_eq!(snapshot.config.daemon.bind, "127.0.0.1:7700");
        assert_eq!(
            metadata.last_applied_reload_safe_keys,
            vec!["retrieval.dense_top_k"]
        );
        assert_eq!(metadata.last_restart_required_keys[0].key, "daemon.bind");
        assert!(metadata
            .last_reload_error
            .as_deref()
            .unwrap_or_default()
            .contains("restart or reindex required"));
    }

    #[tokio::test]
    async fn config_reload_reports_endpoint_queue_capacities_without_applying_them() {
        let test_dir = TestDir::new("config-reload-endpoint-queue-capacity");
        let config_path = test_dir.path().join("config.toml");
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.embedding.endpoint_runtime.queue_capacity = 5;
        config.rerank.endpoint_runtime.queue_capacity = 7;
        config.vision.endpoint_runtime.queue_capacity = 11;
        config.chat.endpoint_runtime.queue_capacity = 13;
        fs::write(&config_path, config.show().unwrap()).unwrap();
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state =
            test_state_with_config_path(config.clone(), test_dir.path(), pipeline, config_path);

        let mut candidate = config.clone();
        candidate.embedding.endpoint_runtime.queue_capacity = 17;
        candidate.rerank.endpoint_runtime.queue_capacity = 19;
        candidate.vision.endpoint_runtime.queue_capacity = 23;
        candidate.chat.endpoint_runtime.queue_capacity = 29;
        fs::write(&state.config_path, candidate.show().unwrap()).unwrap();

        let metadata = reload_config_from_path(&state).await.unwrap();
        let snapshot = runtime_config_snapshot(&state).unwrap();
        let restart_required_keys = metadata
            .last_restart_required_keys
            .iter()
            .map(|change| change.key.as_str())
            .collect::<Vec<_>>();

        assert!(metadata.last_applied_reload_safe_keys.is_empty());
        assert_eq!(
            restart_required_keys,
            vec![
                "chat.queue_capacity",
                "embedding.queue_capacity",
                "rerank.queue_capacity",
                "vision.queue_capacity"
            ]
        );
        assert_eq!(snapshot.config.embedding.endpoint_runtime.queue_capacity, 5);
        assert_eq!(snapshot.config.rerank.endpoint_runtime.queue_capacity, 7);
        assert_eq!(snapshot.config.vision.endpoint_runtime.queue_capacity, 11);
        assert_eq!(snapshot.config.chat.endpoint_runtime.queue_capacity, 13);
        assert!(metadata
            .last_reload_error
            .as_deref()
            .unwrap_or_default()
            .contains("restart or reindex required"));
    }

    #[test]
    fn config_watch_event_ignores_access_events_for_active_config_path() {
        let (_test_dir, state) = config_watch_test_state("config-watch-access");
        let mut event = notify::Event::new(EventKind::Access(notify::event::AccessKind::Close(
            notify::event::AccessMode::Read,
        )));
        event.paths.push(state.config_path.clone());

        assert!(!config_watch_event_matches(
            &Ok(event),
            &state.config_path,
            &state
        ));
    }

    #[test]
    fn config_watch_event_matches_modify_events_for_active_config_path() {
        let (_test_dir, state) = config_watch_test_state("config-watch-modify");
        let mut event = notify::Event::new(EventKind::Modify(notify::event::ModifyKind::Data(
            notify::event::DataChange::Content,
        )));
        event.paths.push(state.config_path.clone());

        assert!(config_watch_event_matches(
            &Ok(event),
            &state.config_path,
            &state
        ));
    }

    #[test]
    fn err_response_includes_upstream_failure_diagnostic() {
        let body = r#"{"error":"bad","token":"fixture-token","api_key":"fixture-api-key"}"#;
        let diagnostic = verbatim_core::upstream::UpstreamFailureDiagnostic {
            phase: "chat".into(),
            client_kind: "chat".into(),
            provider: Some("openai_compatible".into()),
            model: Some("fixture-model".into()),
            base_url_host: Some("example.test".into()),
            http_method: "POST".into(),
            endpoint_path: "/v1/chat/completions".into(),
            status_code: Some(502),
            response_content_type: Some("application/json".into()),
            response_body_available: true,
            response_body_prefix: Some(verbatim_core::upstream::sanitize_text(body)),
            response_body_truncated: false,
            response_body_bytes: Some(body.len() as u64),
            transport_error_kind: Some("http_status_502".into()),
            request_duration_ms: Some(12),
            retry_count: None,
            upstream_request_id: Some("req-fixture".into()),
        };
        let (_, Json(response)) = err(
            StatusCode::BAD_GATEWAY,
            verbatim_core::upstream::UpstreamFailureError::new("chat failed", diagnostic).into(),
        );

        let encoded = serde_json::to_string(&response).unwrap();
        let upstream = response
            .upstream_failure
            .expect("upstream diagnostic is attached");
        assert_eq!(upstream["client_kind"], "chat");
        assert_eq!(upstream["endpoint_path"], "/v1/chat/completions");
        let prefix = upstream["response_body_prefix"].as_str().unwrap();
        let prefix: serde_json::Value = serde_json::from_str(prefix).unwrap();
        assert_eq!(prefix["token"], "<redacted>");
        assert_eq!(prefix["api_key"], "<redacted>");
        assert!(!encoded.contains("fixture-token"));
        assert!(!encoded.contains("fixture-api-key"));
    }

    #[test]
    fn upstream_failure_context_adds_task_request_fields() {
        let task_id = TaskId("task-fixture".into());
        let task = verbatim_core::task::TaskSummary {
            id: task_id.clone(),
            kind: TaskKind::Ask,
            status: TaskStatus::Running,
            created_at: "1".into(),
            updated_at: "1".into(),
            started_at: Some("1".into()),
            finished_at: None,
            request: serde_json::json!({
                "source_id": "src-fixture",
                "embedding_profile_id": "profile-fixture",
            }),
            result: None,
            error: None,
            queue_position: None,
            blocking_reason: None,
            progress: None,
        };

        let enriched = upstream_failure_with_task_context(
            serde_json::json!({"client_kind": "embedding"}),
            &task_id,
            Some(&task),
        );

        assert_eq!(enriched["task_id"], "task-fixture");
        assert_eq!(enriched["task_kind"], "ask");
        assert_eq!(enriched["source_id"], "src-fixture");
        assert_eq!(enriched["embedding_profile_id"], "profile-fixture");
    }

    #[tokio::test]
    async fn task_wait_snapshot_includes_running_progress() {
        let test_dir = TestDir::new("task-progress-snapshot");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let task_id = create_persisted_task(
            &state,
            TaskKind::Ask,
            ask_request_metadata("What is cited?", None, None, false, false),
        )
        .await
        .unwrap();

        ensure_task_started(&state, &task_id).await.unwrap();
        record_task_progress(
            &state,
            &task_id,
            TaskProgressSnapshot::phase("chat")
                .with_counter("chat_bytes_streamed", 16, None)
                .with_endpoint(
                    TaskEndpointSummary::single_call("chat", 1200).with_first_token_latency_ms(250),
                )
                .with_active_worker_kind("ask")
                .with_recent_status("streaming"),
        )
        .await;

        let wait = task_wait_snapshot(&state, task_id, None, 10).await.unwrap();

        assert!(!wait.terminal);
        assert!(wait.spans.is_empty());
        assert!(wait
            .events
            .iter()
            .any(|event| event.event_type == "progress"));
        let progress = wait.task.progress.expect("running task progress");
        assert_eq!(progress.phase.unwrap().name, "chat");
        assert_eq!(progress.counters[0].name, "chat_bytes_streamed");
        assert_eq!(progress.endpoints[0].first_token_latency_ms, Some(250));
    }

    #[tokio::test]
    async fn ingest_task_success_records_terminalize_stage_span() {
        let test_dir = TestDir::new("ingest-task-terminalize-span");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let task_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, true),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &task_id).await.unwrap();

        finish_task_success(
            &state,
            &task_id,
            ingest_result_metadata(1, &EmbeddingCacheStats::default()),
        )
        .await
        .unwrap();

        let response = task_summary_response(&state, task_id).await.unwrap();
        assert!(response
            .spans
            .iter()
            .any(|span| span.phase == IngestTaskStage::TaskTerminalize.as_str()));
    }

    #[tokio::test]
    async fn retrieve_local_spans_are_public_task_spans() {
        let test_dir = TestDir::new("retrieve-local-task-spans");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let task_id = create_persisted_task(
            &state,
            TaskKind::Retrieve,
            retrieve_request_metadata("What is cited?", None, None, 1, 1, 1),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &task_id).await.unwrap();

        record_retrieve_local_spans(
            &state,
            &task_id,
            "1",
            &RetrievalLocalSpansMs {
                query_embedding_ms: 2,
                dense_vector_search_ms: 3,
                vector_queue_wait_ms: Some(15),
                vector_service_ms: Some(16),
                bm25_search_ms: 4,
                rrf_fusion_ms: 5,
                debug_candidate_pack_ms: 6,
                rerank_total_ms: 7,
                result_hydration_ms: 8,
                graph_expansion_ms: 9,
                final_evidence_pack_ms: 10,
                display_evidence_pack_ms: 11,
                response_formatting_ms: 12,
                canonical_support_embedding_ms: Some(13),
                canonical_display_selection_ms: Some(14),
                ..RetrievalLocalSpansMs::default()
            },
        )
        .await
        .unwrap();

        let absent_entries = retrieve_local_span_entries(&RetrievalLocalSpansMs::default());
        assert!(!absent_entries
            .iter()
            .any(|(phase, _, _)| phase.starts_with("vector_")));
        let response = task_summary_response(&state, task_id).await.unwrap();
        assert!(response
            .spans
            .iter()
            .any(|span| span.phase == "retrieve.local.dense_vector_search"
                && span.duration_ms == 3
                && span.metadata["debug_field"] == serde_json::json!("dense_vector_search_ms")));
        assert!(response.spans.iter().any(|span| {
            span.phase == "retrieve.local.vector_queue_wait"
                && span.duration_ms == 15
                && span.metadata["debug_field"] == serde_json::json!("vector_queue_wait_ms")
        }));
        assert!(response.spans.iter().any(|span| {
            span.phase == "retrieve.local.vector_service"
                && span.duration_ms == 16
                && span.metadata["debug_field"] == serde_json::json!("vector_service_ms")
        }));
        assert!(response
            .spans
            .iter()
            .any(|span| span.phase == "retrieve.local.response_formatting"
                && span.duration_ms == 12
                && span.metadata["debug_field"] == serde_json::json!("response_formatting_ms")));
        assert!(response.spans.iter().any(|span| {
            span.phase == "retrieve.local.canonical_display_selection"
                && span.duration_ms == 14
                && span.metadata["nested"] == serde_json::json!(true)
        }));
    }

    #[tokio::test]
    async fn public_task_telemetry_redacts_path_derived_source_id_and_vectors() {
        let test_dir = TestDir::new("task-telemetry-redacts-path-derived-source-id");
        let marker = "issue-160-secret-path-marker";
        let fake_vector_value = "ISSUE160_FAKE_VECTOR_VALUE";
        let sensitive_text = "ISSUE160_DOCUMENT_TEXT_SENTINEL";
        let source_path = test_dir.path().join(format!("{marker}.md"));
        fs::write(&source_path, sensitive_text).unwrap();
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        assert!(
            source_id.0.contains(marker),
            "test must use the production path-derived source id"
        );
        let state = test_state(config, test_dir.path(), pipeline);
        let task_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(
                Some(source_id.0.as_str()),
                false,
                None,
                false,
                true,
            ),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &task_id).await.unwrap();
        record_task_progress(
            &state,
            &task_id,
            TaskProgressSnapshot::phase(IngestTaskStage::Parse.as_str())
                .with_recent_status(format!("parsing source {}", source_id.0)),
        )
        .await;
        record_task_event(
            &state,
            &task_id,
            "diagnostic",
            "task diagnostic",
            serde_json::json!({
                "source_id": source_id.0.as_str(),
                "source_path": source_path.display().to_string(),
                "embedding_vector": [fake_vector_value],
                "text": sensitive_text,
            }),
        )
        .await
        .unwrap();
        record_task_span(
            &state,
            &task_id,
            PhaseTiming::start(IngestTaskStage::EmbeddingPostprocess.as_str()).finish(
                serde_json::json!({
                    "source_id": source_id.0.as_str(),
                    "source_path": source_path.display().to_string(),
                    "embedding_vector": [fake_vector_value],
                    "text": sensitive_text,
                    "chunk_count": 1,
                }),
            ),
        )
        .await
        .unwrap();

        let show = task_summary_response(&state, task_id.clone())
            .await
            .unwrap();
        assert_eq!(
            show.task.request["source_id"],
            serde_json::Value::String(TASK_TELEMETRY_REDACTED.into())
        );
        assert_eq!(
            show.spans[0].metadata["source_id"],
            serde_json::Value::String(TASK_TELEMETRY_REDACTED.into())
        );
        assert_eq!(show.spans[0].metadata["embedding_vector"]["redacted"], true);
        assert_eq!(
            show.task
                .progress
                .as_ref()
                .and_then(|progress| progress.recent_status.as_deref()),
            Some("parsing source <redacted>")
        );
        let Json(list) = list_tasks_handler(
            State(Arc::clone(&state)),
            Query(TaskListQuery {
                status: Some("all".into()),
                limit: Some(10),
            }),
        )
        .await
        .unwrap();
        let events = task_events_response(&state, task_id.clone(), None, Some(10))
            .await
            .unwrap();
        let wait = task_wait_snapshot(&state, task_id, None, 10).await.unwrap();
        let encoded = serde_json::to_string(&(show, list, events, wait)).unwrap();

        for forbidden in [
            marker,
            source_id.0.as_str(),
            source_path.to_str().unwrap(),
            fake_vector_value,
            sensitive_text,
        ] {
            assert!(
                !encoded.contains(forbidden),
                "public task telemetry leaked {forbidden}: {encoded}"
            );
        }
        assert!(encoded.contains(TASK_TELEMETRY_REDACTED));
    }

    #[tokio::test]
    async fn public_all_source_ingest_redacts_path_derived_source_id_from_progress() {
        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("all-source-ingest-redacts-path-derived-source-id");
        let marker = "issue-160-all-source-secret-path-marker";
        let sensitive_text = "ISSUE160_ALL_SOURCE_DOCUMENT_TEXT_SENTINEL";
        let source_path = test_dir.path().join(format!("{marker}.md"));
        fs::write(
            &source_path,
            format!("# Heading\n\n{sensitive_text} all-source retrieval body\n"),
        )
        .unwrap();
        let config = retrieve_test_config(&model_server.base_url);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        assert!(
            source_id.0.contains(marker),
            "test must use the production path-derived source id"
        );
        let state = test_state(config, test_dir.path(), pipeline);

        let Json(response) = ingest_all(
            State(Arc::clone(&state)),
            Query(IngestQuery {
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.ingested, 1);
        let Json(list) = list_tasks_handler(
            State(Arc::clone(&state)),
            Query(TaskListQuery {
                status: Some("all".into()),
                limit: Some(10),
            }),
        )
        .await
        .unwrap();
        let task_id = list
            .tasks
            .iter()
            .find(|task| task.kind == TaskKind::Ingest)
            .expect("all-source ingest task should be listed")
            .id
            .clone();
        let show = task_summary_response(&state, task_id.clone())
            .await
            .unwrap();
        let events = task_events_response(&state, task_id.clone(), None, Some(100))
            .await
            .unwrap();
        let wait = task_wait_snapshot(&state, task_id, None, 100)
            .await
            .unwrap();
        let encoded = serde_json::to_string(&(show, list, events, wait)).unwrap();

        for forbidden in [
            marker,
            source_id.0.as_str(),
            source_path.to_str().unwrap(),
            sensitive_text,
        ] {
            assert!(
                !encoded.contains(forbidden),
                "all-source public task telemetry leaked {forbidden}: {encoded}"
            );
        }
        assert!(encoded.contains(TASK_TELEMETRY_REDACTED));
        assert!(encoded.contains("parsing source"));
        assert!(encoded.contains("ingesting source"));
        assert!(encoded.contains("finished source"));
    }

    #[tokio::test]
    async fn task_list_defaults_to_active_tasks_with_queue_details() {
        let test_dir = TestDir::new("task-list-active");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let done_id = create_persisted_task(
            &state,
            TaskKind::Ask,
            ask_request_metadata("What is cited?", None, None, false, false),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &done_id).await.unwrap();
        finish_task_success(
            &state,
            &done_id,
            ask_result_metadata("answer", 0, false, false),
        )
        .await
        .unwrap();
        let (running_id, queued_id) = blocked_ingest_pair(&state).await;

        let Json(response) = list_tasks_handler(
            State(Arc::clone(&state)),
            Query(TaskListQuery {
                status: None,
                limit: None,
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.total, 2);
        assert_eq!(response.tasks.len(), 2);
        assert!(response.tasks.iter().any(|task| task.id == running_id));
        let queued = response
            .tasks
            .iter()
            .find(|task| task.id == queued_id)
            .expect("queued task is listed");
        assert_eq!(queued.queue_position, Some(1));
        assert_eq!(
            queued.blocking_reason.as_deref(),
            Some("waiting for running ingest task to finish")
        );
    }

    #[tokio::test]
    async fn sqlite_writer_activity_does_not_block_task_status_read() {
        let test_dir = TestDir::new("writer-active-task-status-read");
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.daemon.resources.sqlite_writer_concurrency = 32;
        config.daemon.resources.sqlite_reader_concurrency = 32;
        assert_eq!(
            sqlite_writer_resource_limits(&config.daemon.resources.bounded()).capacity,
            SQLITE_WRITER_ACTIVE_CAPACITY
        );
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let task_id = create_persisted_task(
            &state,
            TaskKind::Ask,
            ask_request_metadata("What is cited?", None, None, false, false),
        )
        .await
        .unwrap();
        let writer_permit = state
            .resources
            .sqlite_writer
            .acquire()
            .await
            .expect("writer permit");

        let response = tokio::time::timeout(
            Duration::from_millis(250),
            task_summary_response(&state, task_id.clone()),
        )
        .await
        .expect("task status read does not wait behind writer resource")
        .unwrap();

        assert_eq!(response.task.id, task_id);
        let Json(health) = health(State(Arc::clone(&state))).await;
        let writer = health
            .resources
            .iter()
            .find(|resource| resource.name == "sqlite_writer")
            .expect("sqlite writer resource is reported");
        assert_eq!(writer.capacity, SQLITE_WRITER_ACTIVE_CAPACITY);
        assert!(writer.active >= 1);
        let reader = health
            .resources
            .iter()
            .find(|resource| resource.name == "sqlite_reader")
            .expect("sqlite reader resource is reported");
        assert!(reader.completed >= 1);
        let vector_search = health
            .resources
            .iter()
            .find(|resource| resource.name == "vector_search")
            .expect("vector search resource is reported");
        assert_eq!(vector_search.kind, "vector_search");
        drop(writer_permit);
    }

    #[tokio::test]
    async fn health_reports_idle_reclaim_disabled_by_default() {
        let test_dir = TestDir::new("idle-reclaim-default-health");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let Json(health) = health(State(Arc::clone(&state))).await;
        let reclaim = health.idle_reclaim.expect("idle reclaim state is reported");

        assert!(!reclaim.enabled);
        assert!(!reclaim.eligible);
        assert_eq!(reclaim.skip_reason.as_deref(), Some("disabled"));
        assert_eq!(reclaim.active.http_requests, 0);
        assert_eq!(reclaim.active.active_tasks, 0);
    }

    #[tokio::test]
    async fn health_reports_starting_then_ready_readiness() {
        let test_dir = TestDir::new("health-readiness");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        set_readiness(
            &state,
            ReadinessHealth::starting(
                "orphan_recovery",
                Some("recovering previous running ingest tasks".into()),
            ),
        );
        let Json(starting) = health(State(Arc::clone(&state))).await;
        assert_eq!(starting.status, "ok");
        assert!(starting.readiness.process_alive);
        assert_eq!(starting.readiness.state, "starting");
        assert!(!starting.readiness.retrieval_ready);
        assert_eq!(starting.readiness.startup_phase, "orphan_recovery");
        assert_eq!(
            starting.readiness.degraded_reason.as_deref(),
            Some("recovering previous running ingest tasks")
        );

        set_readiness(&state, ReadinessHealth::ready());
        let Json(ready) = health(State(Arc::clone(&state))).await;
        assert_eq!(ready.readiness.state, "ready");
        assert!(ready.readiness.retrieval_ready);
        assert_eq!(ready.readiness.startup_phase, "ready");
        assert!(ready.readiness.degraded_reason.is_none());
    }

    #[tokio::test]
    async fn health_http_is_available_while_startup_job_is_blocked() {
        let test_dir = TestDir::new("health-startup-blocked");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        set_readiness(
            &state,
            ReadinessHealth::starting(
                "startup_maintenance",
                Some("startup maintenance is still running".into()),
            ),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let app = Router::new()
            .route("/api/health", get(health))
            .with_state(Arc::clone(&state));
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    while !*shutdown_rx.borrow() {
                        if shutdown_rx.changed().await.is_err() {
                            break;
                        }
                    }
                })
                .await;
        });
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let startup_state = Arc::clone(&state);
        let startup_job = tokio::spawn(async move {
            release_rx.await.unwrap();
            set_readiness(&startup_state, ReadinessHealth::ready());
        });

        let starting = http_get_health_for_test(addr).await;
        assert_eq!(starting.readiness.state, "starting");
        assert!(!starting.readiness.retrieval_ready);

        release_tx.send(()).unwrap();
        startup_job.await.unwrap();
        let ready = http_get_health_for_test(addr).await;
        assert_eq!(ready.readiness.state, "ready");
        assert!(ready.readiness.retrieval_ready);

        shutdown_tx.send(true).unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn startup_race_returns_when_server_exits_before_startup_finishes() {
        let (server_exit_tx, server_exit_rx) = tokio::sync::oneshot::channel();
        let mut server_task = tokio::spawn(async move {
            server_exit_rx.await.unwrap();
            Ok(())
        });
        let startup = std::future::pending::<()>();

        server_exit_tx.send(()).unwrap();
        let outcome = tokio::time::timeout(
            Duration::from_millis(250),
            await_startup_or_server_exit(startup, &mut server_task),
        )
        .await
        .expect("server exit should win without waiting for startup")
        .expect("server task should exit cleanly");

        assert!(matches!(outcome, DaemonStartupRace::ServerExited));
    }

    #[tokio::test]
    async fn retrieve_starting_returns_typed_startup_error() {
        let test_dir = TestDir::new("retrieve-starting");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        set_readiness(
            &state,
            ReadinessHealth::starting(
                "orphan_recovery",
                Some("recovering previous running ingest tasks".into()),
            ),
        );

        let (status, Json(error)) = retrieve(
            State(Arc::clone(&state)),
            Json(RetrieveRequest {
                question: "question".into(),
                source_id: None,
                collection_filter: CollectionFilterRequest::default(),
                embedding_profile_id: None,
                limit: None,
                page_size: None,
                page: None,
                fast: false,
                rerank: None,
                dense_top_k: None,
                bm25_top_k: None,
                rerank_top_n: None,
                bypass_cache: false,
                include_debug: false,
                include_debug_packs: false,
                include_locator: false,
                passage: false,
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_eq!(error.code.as_deref(), Some("retrieval_not_ready"));
        assert_eq!(error.readiness.as_deref(), Some("starting"));
        assert_eq!(error.retrieval_ready, Some(false));
        assert_eq!(error.startup_phase.as_deref(), Some("orphan_recovery"));
        assert_eq!(
            error.degraded_reason.as_deref(),
            Some("recovering previous running ingest tasks")
        );
        assert!(error.error.contains("verbatim daemon is starting"));
        assert!(error.error.contains("retrieval is not ready"));
    }

    #[tokio::test]
    async fn ask_stream_starting_returns_json_service_unavailable() {
        let test_dir = TestDir::new("ask-stream-starting");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        set_readiness(
            &state,
            ReadinessHealth::starting(
                "orphan_recovery",
                Some("recovering previous running ingest tasks".into()),
            ),
        );

        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        let (shutdown_tx, mut shutdown_rx) = watch::channel(false);
        let app = Router::new()
            .route("/api/ask/stream", post(ask_stream))
            .with_state(Arc::clone(&state));
        let server = tokio::spawn(async move {
            let _ = axum::serve(listener, app)
                .with_graceful_shutdown(async move {
                    while !*shutdown_rx.borrow() {
                        if shutdown_rx.changed().await.is_err() {
                            break;
                        }
                    }
                })
                .await;
        });

        let (status_line, headers, body) = http_post_ask_stream_for_test(addr).await;
        assert!(
            status_line.starts_with("HTTP/1.1 503 Service Unavailable"),
            "unexpected response status: {status_line}"
        );
        let lower_headers = headers.to_ascii_lowercase();
        assert!(lower_headers.contains("content-type: application/json"));
        assert!(!lower_headers.contains("text/event-stream"));

        let error: ErrorResponse = serde_json::from_str(&body).unwrap();
        assert_eq!(error.code.as_deref(), Some("retrieval_not_ready"));
        assert_eq!(error.readiness.as_deref(), Some("starting"));
        assert_eq!(error.retrieval_ready, Some(false));
        assert_eq!(error.startup_phase.as_deref(), Some("orphan_recovery"));
        assert_eq!(
            error.degraded_reason.as_deref(),
            Some("recovering previous running ingest tasks")
        );
        assert!(error.error.contains("verbatim daemon is starting"));
        assert!(error.error.contains("retrieval is not ready"));

        shutdown_tx.send(true).unwrap();
        server.await.unwrap();
    }

    #[tokio::test]
    async fn background_ingest_starting_rejects_without_persisting_task() {
        let test_dir = TestDir::new("background-ingest-starting");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        set_readiness(
            &state,
            ReadinessHealth::starting(
                "initializing_pipeline",
                Some("initializing indexes and startup maintenance".into()),
            ),
        );

        let (status, Json(error)) = submit_ingest_task(
            State(Arc::clone(&state)),
            Json(TaskIngestRequest {
                source_id: None,
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_starting_readiness_error(&error, "initializing_pipeline");
        assert_eq!(task_count_for_test(&state), 0);
    }

    #[tokio::test]
    async fn background_reindex_starting_rejects_without_persisting_task() {
        let test_dir = TestDir::new("background-reindex-starting");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        set_readiness(
            &state,
            ReadinessHealth::starting(
                "initializing_pipeline",
                Some("initializing indexes and startup maintenance".into()),
            ),
        );

        let (status, Json(error)) = submit_reindex_task(
            State(Arc::clone(&state)),
            Json(ReindexRequest {
                source_id: None,
                all: true,
                stale: false,
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        assert_starting_readiness_error(&error, "initializing_pipeline");
        assert_eq!(task_count_for_test(&state), 0);
    }

    #[tokio::test]
    async fn idle_exit_health() {
        let test_dir = TestDir::new("idle-exit-health");
        let config = idle_exit_test_config("http://127.0.0.1:9/v1", 1);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_exit_timeout_elapsed(&state, 5);

        let Json(health) = health(State(Arc::clone(&state))).await;
        let exit = health.idle_exit.expect("idle exit state is reported");

        assert!(exit.enabled);
        assert_eq!(exit.timeout_millis, 1_000);
        assert!(exit.deadline_unix_ms >= exit.last_activity_unix_ms);
        assert_eq!(exit.active.http_requests, 0);
        assert_eq!(exit.active.watched_roots, 0);
    }

    #[tokio::test]
    async fn idle_exit_tracks_http_and_sse_activity() {
        let test_dir = TestDir::new("idle-exit-http-sse");
        let mut config = idle_exit_test_config("http://127.0.0.1:9/v1", 60);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config.clone(), test_dir.path(), pipeline);

        assert!(idle_exit_tracks_request_path(
            &state.runtime_config.read().unwrap().config.daemon.idle_exit,
            "/api/retrieve"
        ));
        assert!(!idle_exit_tracks_request_path(
            &state.runtime_config.read().unwrap().config.daemon.idle_exit,
            "/api/health"
        ));

        let before_health = state.idle_exit.last_activity_unix_ms();
        let Json(_) = health(State(Arc::clone(&state))).await;
        assert_eq!(state.idle_exit.last_activity_unix_ms(), before_health);

        let counted_health_dir = TestDir::new("idle-exit-counted-health");
        config.daemon.idle_exit.count_health_requests = true;
        let pipeline = IngestPipeline::new(&config, counted_health_dir.path()).unwrap();
        let state = test_state(config, counted_health_dir.path(), pipeline);
        assert!(idle_exit_tracks_request_path(
            &state.runtime_config.read().unwrap().config.daemon.idle_exit,
            "/api/health"
        ));
        force_idle_exit_timeout_elapsed(&state, 120);
        let before_counted_health = state.idle_exit.last_activity_unix_ms();
        {
            let _health_guard = state.idle_exit.start_http();
            let exit = idle_exit_gate(&state, idle_exit_resource_snapshots_for_test(&state)).health;
            assert_eq!(exit.active.http_requests, 1);
            assert_eq!(exit.skip_reason.as_deref(), Some("active_http_requests"));
        }
        assert!(state.idle_exit.last_activity_unix_ms() >= before_counted_health);

        force_idle_exit_timeout_elapsed(&state, 120);
        {
            let _sse_guard = state.idle_exit.start_sse();
            let exit = idle_exit_gate(&state, idle_exit_resource_snapshots_for_test(&state)).health;
            assert_eq!(exit.active.sse_streams, 1);
            assert_eq!(exit.skip_reason.as_deref(), Some("active_sse_streams"));
        }
        let exit = idle_exit_gate(&state, idle_exit_resource_snapshots_for_test(&state)).health;
        assert_eq!(exit.active.sse_streams, 0);
        assert_eq!(
            exit.skip_reason.as_deref(),
            Some("idle_timeout_not_reached")
        );
    }

    #[tokio::test]
    async fn idle_exit_exits_cleanly_after_timeout() {
        let test_dir = TestDir::new("idle-exit-clean-timeout");
        let config = idle_exit_test_config("http://127.0.0.1:9/v1", 1);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        assert!(!run_idle_exit_cycle_if_due_with_resources(
            &state,
            idle_exit_resource_snapshots_for_test(&state),
            || idle_exit_resource_snapshots_for_test(&state)
        ));
        force_idle_exit_timeout_elapsed(&state, 5);

        assert!(run_idle_exit_cycle_if_due_with_resources(
            &state,
            idle_exit_resource_snapshots_for_test(&state),
            || idle_exit_resource_snapshots_for_test(&state)
        ));
        assert!(state.idle_exit.shutdown_requested.load(Ordering::Acquire));
        assert!(!run_idle_exit_cycle_if_due_with_resources(
            &state,
            idle_exit_resource_snapshots_for_test(&state),
            || idle_exit_resource_snapshots_for_test(&state)
        ));
    }

    #[tokio::test]
    async fn idle_exit_blocks_on_tasks_resources_ingest_and_pipeline() {
        let test_dir = TestDir::new("idle-exit-blockers");
        let config = idle_exit_test_config("http://127.0.0.1:9/v1", 1);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_exit_timeout_elapsed(&state, 5);

        let task_id = create_persisted_task(
            &state,
            TaskKind::Retrieve,
            retrieve_request_metadata("question", None, None, 1, 1, 1),
        )
        .await
        .unwrap();
        let exit = idle_exit_gate(&state, idle_exit_resource_snapshots_for_test(&state)).health;
        assert_eq!(exit.skip_reason.as_deref(), Some("active_tasks"));
        finish_task_success(&state, &task_id, serde_json::json!({"returned_results": 0}))
            .await
            .unwrap();
        let exit = idle_exit_gate(&state, idle_exit_resource_snapshots_for_test(&state)).health;
        assert_eq!(
            exit.skip_reason.as_deref(),
            Some("idle_timeout_not_reached")
        );

        force_idle_exit_timeout_elapsed(&state, 5);
        let exit = idle_exit_gate(&state, vec![idle_exit_resource_snapshot_for_test(1, 0)]).health;
        assert_eq!(exit.skip_reason.as_deref(), Some("active_resources"));

        force_idle_exit_timeout_elapsed(&state, 5);
        let exit = idle_exit_gate(&state, vec![idle_exit_resource_snapshot_for_test(0, 1)]).health;
        assert_eq!(exit.skip_reason.as_deref(), Some("queued_resources"));

        force_idle_exit_timeout_elapsed(&state, 5);
        state.ingest_queue_active.store(true, Ordering::Release);
        let exit = idle_exit_gate(&state, idle_exit_resource_snapshots_for_test(&state)).health;
        assert_eq!(exit.skip_reason.as_deref(), Some("ingest_queue_active"));
        state.ingest_queue_active.store(false, Ordering::Release);

        force_idle_exit_timeout_elapsed(&state, 5);
        state.ingest_worker_active.store(true, Ordering::Release);
        let exit = idle_exit_gate(&state, idle_exit_resource_snapshots_for_test(&state)).health;
        assert_eq!(exit.skip_reason.as_deref(), Some("ingest_worker_active"));
        state.ingest_worker_active.store(false, Ordering::Release);

        force_idle_exit_timeout_elapsed(&state, 5);
        let pipeline = take_pipeline(&state).expect("take pipeline slot");
        let exit = idle_exit_gate(&state, idle_exit_resource_snapshots_for_test(&state)).health;
        assert_eq!(exit.skip_reason.as_deref(), Some("pipeline_busy"));
        restore_pipeline(&state, pipeline).expect("restore pipeline slot");
    }

    #[tokio::test]
    async fn idle_exit_collection_watcher_safety() {
        let test_dir = TestDir::new("idle-exit-watcher-safety");
        let mut config = idle_exit_test_config("http://127.0.0.1:9/v1", 1);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config.clone(), test_dir.path(), pipeline);
        force_idle_exit_timeout_elapsed(&state, 5);
        update_collection_watcher_status(&state, "articles", |status| {
            status.active = true;
            status.watched_root_count = 2;
        });
        let exit = idle_exit_gate(&state, idle_exit_resource_snapshots_for_test(&state)).health;
        assert_eq!(
            exit.skip_reason.as_deref(),
            Some("active_collection_watchers")
        );
        assert_eq!(exit.active.watched_roots, 2);

        config.daemon.idle_exit.allow_with_collection_watcher = true;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_exit_timeout_elapsed(&state, 5);
        update_collection_watcher_status(&state, "articles", |status| {
            status.active = true;
            status.watched_root_count = 2;
        });
        let exit = idle_exit_gate(&state, idle_exit_resource_snapshots_for_test(&state)).health;
        assert!(exit.eligible);
        assert_eq!(exit.skip_reason, None);

        update_collection_watcher_status(&state, "articles", |status| {
            status.pending_event_count = 1;
        });
        let exit = idle_exit_gate(&state, idle_exit_resource_snapshots_for_test(&state)).health;
        assert_eq!(
            exit.skip_reason.as_deref(),
            Some("pending_collection_watcher_events")
        );

        let (tx, mut rx) = mpsc::channel(1);
        set_collection_watcher_sender(&state, tx);
        queue_idle_exit_collection_watcher_resync_if_enabled(&state);
        match rx.try_recv().expect("resync command queued") {
            CollectionWatcherCommand::ResyncActive => {}
            other => panic!("unexpected collection watcher command: {other:?}"),
        }
    }

    #[tokio::test]
    async fn idle_exit_blocks_when_required_resync_cannot_be_queued() {
        let test_dir = TestDir::new("idle-exit-watcher-resync-send-fail");
        let mut config = idle_exit_test_config("http://127.0.0.1:9/v1", 1);
        config.daemon.idle_exit.allow_with_collection_watcher = true;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_exit_timeout_elapsed(&state, 5);
        update_collection_watcher_status(&state, "articles", |status| {
            status.active = true;
            status.watched_root_count = 2;
        });

        queue_idle_exit_collection_watcher_resync_if_enabled(&state);
        let exit = idle_exit_gate(&state, idle_exit_resource_snapshots_for_test(&state)).health;

        assert_eq!(exit.active.pending_watcher_events, 1);
        assert_eq!(
            exit.skip_reason.as_deref(),
            Some("pending_collection_watcher_events")
        );
    }

    #[tokio::test]
    async fn idle_exit_keeps_failed_watcher_maintenance_pending() {
        let test_dir = TestDir::new("idle-exit-watcher-maintenance-failure-pending");
        let mut config = idle_exit_test_config("http://127.0.0.1:9/v1", 1);
        config.daemon.idle_exit.allow_with_collection_watcher = true;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_exit_timeout_elapsed(&state, 5);
        update_collection_watcher_status(&state, "missing", |status| {
            status.active = true;
            status.watched_root_count = 1;
            status.pending_event_count = 1;
        });
        let mut debounced = DebouncedCollectionSet::default();
        assert!(debounced.insert_many(["missing".to_string()]));

        flush_collection_watcher_debounce(&state, &mut debounced).await;
        let exit = idle_exit_gate(&state, idle_exit_resource_snapshots_for_test(&state)).health;

        assert_eq!(exit.active.pending_watcher_events, 1);
        assert_eq!(
            exit.skip_reason.as_deref(),
            Some("pending_collection_watcher_events")
        );
        let last_error = state
            .collection_watcher
            .statuses
            .lock()
            .unwrap()
            .get("missing")
            .and_then(|status| status.last_error.clone())
            .expect("maintenance error recorded");
        assert!(last_error.contains("collection not found"));
    }

    #[tokio::test]
    async fn idle_exit_keeps_failed_watcher_resync_pending() {
        let test_dir = TestDir::new("idle-exit-watcher-resync-failure-pending");
        let mut config = idle_exit_test_config("http://127.0.0.1:9/v1", 1);
        config.daemon.idle_exit.allow_with_collection_watcher = true;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_exit_timeout_elapsed(&state, 5);
        update_collection_watcher_status(&state, "missing", |status| {
            status.active = true;
            status.watched_root_count = 1;
        });
        state
            .idle_exit
            .watcher_resync_requested
            .store(true, Ordering::Release);

        resync_active_collection_watchers(&state).await;
        let exit = idle_exit_gate(&state, idle_exit_resource_snapshots_for_test(&state)).health;

        assert!(state
            .idle_exit
            .watcher_resync_requested
            .load(Ordering::Acquire));
        assert_eq!(exit.active.pending_watcher_events, 1);
        assert_eq!(
            exit.skip_reason.as_deref(),
            Some("pending_collection_watcher_events")
        );
    }

    #[tokio::test]
    async fn idle_exit_shutdown_requested_rejects_new_real_requests() {
        let test_dir = TestDir::new("idle-exit-shutdown-admission");
        let config = idle_exit_test_config("http://127.0.0.1:9/v1", 1);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        assert_eq!(
            idle_exit_shutdown_rejection_status(&state, "/api/tasks/ingest"),
            None
        );
        state
            .idle_exit
            .shutdown_requested
            .store(true, Ordering::Release);
        assert_eq!(
            idle_exit_shutdown_rejection_status(&state, "/api/tasks/ingest"),
            Some(StatusCode::SERVICE_UNAVAILABLE)
        );
        assert_eq!(
            idle_exit_shutdown_rejection_status(&state, "/api/health"),
            None
        );

        let request = state.idle_exit.start_http();
        assert_eq!(state.idle_exit.active_http_requests(), 1);
        assert_eq!(
            idle_exit_shutdown_rejection_status(&state, "/api/tasks/ingest"),
            Some(StatusCode::SERVICE_UNAVAILABLE)
        );
        drop(request);
        assert_eq!(state.idle_exit.active_http_requests(), 0);
    }

    #[tokio::test]
    async fn idle_exit_reopens_admission_when_activity_races_shutdown_confirmation() {
        let test_dir = TestDir::new("idle-exit-admission-race");
        let config = idle_exit_test_config("http://127.0.0.1:9/v1", 1);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_exit_timeout_elapsed(&state, 5);
        state
            .idle_exit
            .shutdown_requested
            .store(true, Ordering::Release);

        let _request = state.idle_exit.start_http();
        let confirmation = confirm_idle_exit_shutdown_after_admission_closed(&state);

        assert!(confirmation.is_none());
        assert!(!state.idle_exit.shutdown_requested.load(Ordering::Acquire));
    }

    #[tokio::test]
    async fn idle_exit_health_snapshot_does_not_refresh_after_blockers_clear() {
        let test_dir = TestDir::new("idle-exit-health-pure");
        let config = idle_exit_test_config("http://127.0.0.1:9/v1", 1);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_exit_timeout_elapsed(&state, 5);

        let task_id = create_persisted_task(
            &state,
            TaskKind::Retrieve,
            retrieve_request_metadata("question", None, None, 1, 1, 1),
        )
        .await
        .unwrap();
        let blocked = idle_exit_gate(&state, idle_exit_resource_snapshots_for_test(&state)).health;
        assert_eq!(blocked.skip_reason.as_deref(), Some("active_tasks"));
        finish_task_success(&state, &task_id, serde_json::json!({"returned_results": 0}))
            .await
            .unwrap();
        force_idle_exit_timeout_elapsed(&state, 5);
        let before_health = state.idle_exit.last_activity_unix_ms();

        let Json(_) = health(State(Arc::clone(&state))).await;

        assert_eq!(state.idle_exit.last_activity_unix_ms(), before_health);
    }

    #[tokio::test]
    async fn idle_exit_blocks_pending_collection_watcher_resync() {
        let test_dir = TestDir::new("idle-exit-watcher-resync-blocker");
        let mut config = idle_exit_test_config("http://127.0.0.1:9/v1", 1);
        config.daemon.idle_exit.allow_with_collection_watcher = true;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_exit_timeout_elapsed(&state, 5);

        state
            .idle_exit
            .watcher_resync_requested
            .store(true, Ordering::Release);
        let exit = idle_exit_gate(&state, idle_exit_resource_snapshots_for_test(&state)).health;
        assert_eq!(exit.active.pending_watcher_events, 1);
        assert_eq!(
            exit.skip_reason.as_deref(),
            Some("pending_collection_watcher_events")
        );

        resync_active_collection_watchers(&state).await;
        force_idle_exit_timeout_elapsed(&state, 5);
        let exit = idle_exit_gate(&state, idle_exit_resource_snapshots_for_test(&state)).health;
        assert_eq!(exit.active.pending_watcher_events, 0);
        assert_ne!(
            exit.skip_reason.as_deref(),
            Some("pending_collection_watcher_events")
        );
    }

    #[tokio::test]
    async fn idle_reclaim_skips_active_http_without_invoking_backends() {
        let test_dir = TestDir::new("idle-reclaim-active-http");
        let config = idle_reclaim_test_config("http://127.0.0.1:9/v1", 1, 60, true, true);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_reclaim_timeout_elapsed(&state, 120);
        let _http = state.idle_reclaim.start_http().await;

        let result = run_idle_reclaim_cycle_if_due(&state).await;

        assert_eq!(result.status, "skipped");
        assert_eq!(result.skip_reason.as_deref(), Some("active_http_requests"));
        assert!(!result.sqlite.attempted);
        assert!(!result.allocator.attempted);
        let Json(health) = health(State(Arc::clone(&state))).await;
        let reclaim = health.idle_reclaim.unwrap();
        assert_eq!(reclaim.active.http_requests, 1);
        assert_eq!(reclaim.skip_reason.as_deref(), Some("active_http_requests"));
    }

    #[tokio::test]
    async fn idle_reclaim_skips_active_sse_without_invoking_backends() {
        let test_dir = TestDir::new("idle-reclaim-active-sse");
        let config = idle_reclaim_test_config("http://127.0.0.1:9/v1", 1, 60, true, true);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_reclaim_timeout_elapsed(&state, 120);
        let _sse = state.idle_reclaim.start_sse().await;

        let result = run_idle_reclaim_cycle_if_due(&state).await;

        assert_eq!(result.status, "skipped");
        assert_eq!(result.skip_reason.as_deref(), Some("active_sse_streams"));
        assert!(!result.sqlite.attempted);
        assert!(!result.allocator.attempted);
    }

    #[tokio::test]
    async fn idle_reclaim_skips_active_resource_without_invoking_backends() {
        let test_dir = TestDir::new("idle-reclaim-active-resource");
        let config = idle_reclaim_test_config("http://127.0.0.1:9/v1", 1, 60, true, true);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_reclaim_timeout_elapsed(&state, 120);
        let _permit = state
            .resources
            .sqlite_reader
            .acquire()
            .await
            .expect("reader permit");

        let result = run_idle_reclaim_cycle_if_due(&state).await;

        assert_eq!(result.status, "skipped");
        assert_eq!(result.skip_reason.as_deref(), Some("active_resources"));
        assert!(!result.sqlite.attempted);
        assert!(!result.allocator.attempted);
    }

    #[tokio::test]
    async fn idle_reclaim_skips_queued_resource_without_invoking_backends() {
        let test_dir = TestDir::new("idle-reclaim-queued-resource");
        let config = idle_reclaim_test_config("http://127.0.0.1:9/v1", 1, 60, true, true);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_reclaim_timeout_elapsed(&state, 120);

        let result = run_idle_reclaim_initial_gate_for_test(
            &state,
            vec![idle_reclaim_resource_snapshot_for_test(0, 1)],
        );

        assert_idle_reclaim_skipped_without_backends(&result, "queued_resources");
    }

    #[tokio::test]
    async fn idle_reclaim_skips_active_tasks_without_invoking_backends() {
        let test_dir = TestDir::new("idle-reclaim-active-task");
        let config = idle_reclaim_test_config("http://127.0.0.1:9/v1", 1, 60, true, true);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_reclaim_timeout_elapsed(&state, 120);
        let _task_id = create_persisted_task(
            &state,
            TaskKind::Retrieve,
            retrieve_request_metadata("question", None, None, 1, 1, 1),
        )
        .await
        .unwrap();

        let result = run_idle_reclaim_cycle_if_due(&state).await;

        assert_eq!(result.status, "skipped");
        assert_eq!(result.skip_reason.as_deref(), Some("active_tasks"));
        assert!(!result.sqlite.attempted);
        assert!(!result.allocator.attempted);
    }

    #[tokio::test]
    async fn idle_reclaim_waits_after_background_task_completion() {
        let test_dir = TestDir::new("idle-reclaim-after-task-completion");
        let config = idle_reclaim_test_config("http://127.0.0.1:9/v1", 60, 60, true, true);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_reclaim_timeout_elapsed(&state, 120);
        let task_id = create_persisted_task(
            &state,
            TaskKind::Retrieve,
            retrieve_request_metadata("question", None, None, 1, 1, 1),
        )
        .await
        .unwrap();

        finish_task_success(&state, &task_id, serde_json::json!({"returned_results": 0}))
            .await
            .unwrap();
        let immediate = run_idle_reclaim_cycle_if_due(&state).await;

        assert_idle_reclaim_skipped_without_backends(&immediate, "idle_timeout_not_reached");

        force_idle_reclaim_timeout_elapsed(&state, 120);
        let after_timeout = run_idle_reclaim_cycle_if_due(&state).await;

        assert_ne!(after_timeout.status, "skipped");
        assert!(after_timeout.sqlite.attempted);
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        assert!(after_timeout.allocator.attempted);
        #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
        assert!(!after_timeout.allocator.attempted);
    }

    #[tokio::test]
    async fn idle_reclaim_skips_active_ingest_queue_without_invoking_backends() {
        let test_dir = TestDir::new("idle-reclaim-active-ingest-queue");
        let config = idle_reclaim_test_config("http://127.0.0.1:9/v1", 1, 60, true, true);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_reclaim_timeout_elapsed(&state, 120);
        state.ingest_queue_active.store(true, Ordering::Release);

        let result = run_idle_reclaim_cycle_if_due(&state).await;

        assert_idle_reclaim_skipped_without_backends(&result, "ingest_queue_active");
        state.ingest_queue_active.store(false, Ordering::Release);
    }

    #[tokio::test]
    async fn idle_reclaim_skips_busy_pipeline_without_invoking_backends() {
        let test_dir = TestDir::new("idle-reclaim-busy-pipeline");
        let config = idle_reclaim_test_config("http://127.0.0.1:9/v1", 1, 60, true, true);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_reclaim_timeout_elapsed(&state, 120);
        let pipeline = take_pipeline(&state).expect("take pipeline slot");

        let result = run_idle_reclaim_cycle_if_due(&state).await;

        assert_idle_reclaim_skipped_without_backends(&result, "pipeline_busy");
        restore_pipeline(&state, pipeline).expect("restore pipeline slot");
    }

    #[tokio::test]
    async fn idle_reclaim_skips_before_idle_timeout_without_invoking_backends() {
        let test_dir = TestDir::new("idle-reclaim-timeout-not-reached");
        let config = idle_reclaim_test_config("http://127.0.0.1:9/v1", 60, 60, true, true);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let result = run_idle_reclaim_cycle_if_due(&state).await;

        assert_idle_reclaim_skipped_without_backends(&result, "idle_timeout_not_reached");
    }

    #[tokio::test]
    async fn idle_reclaim_runs_sqlite_shrink_and_allocator_trim_when_idle() {
        let test_dir = TestDir::new("idle-reclaim-runs");
        let config = idle_reclaim_test_config("http://127.0.0.1:9/v1", 1, 60, true, true);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_reclaim_timeout_elapsed(&state, 120);

        let result = run_idle_reclaim_cycle_if_due(&state).await;

        assert_ne!(result.status, "skipped");
        assert!(result.sqlite.attempted);
        assert_eq!(result.sqlite.success_count, 2);
        assert_eq!(result.sqlite.failure_count, 0);
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        {
            assert!(result.allocator.attempted);
            assert_eq!(result.allocator.success_count, 1);
            assert_eq!(result.allocator.failure_count, 0);
        }
        #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
        {
            assert!(!result.allocator.attempted);
            assert_eq!(result.allocator.status, "unsupported");
        }
        let Json(health) = health(State(Arc::clone(&state))).await;
        let reclaim = health.idle_reclaim.unwrap();
        assert_eq!(
            reclaim
                .last_result
                .as_ref()
                .map(|last| last.status.as_str()),
            Some(result.status.as_str())
        );
    }

    #[tokio::test]
    async fn idle_reclaim_min_interval_blocks_repeat_attempt() {
        let test_dir = TestDir::new("idle-reclaim-min-interval");
        let config = idle_reclaim_test_config("http://127.0.0.1:9/v1", 1, 600, true, true);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_reclaim_timeout_elapsed(&state, 120);

        let first = run_idle_reclaim_cycle_if_due(&state).await;
        force_idle_reclaim_timeout_elapsed(&state, 120);
        let second = run_idle_reclaim_cycle_if_due(&state).await;

        assert!(first.sqlite.attempted);
        assert_eq!(second.status, "skipped");
        assert_eq!(
            second.skip_reason.as_deref(),
            Some("min_interval_not_reached")
        );
        assert!(!second.sqlite.attempted);
        assert!(!second.allocator.attempted);
        let Json(health) = health(State(Arc::clone(&state))).await;
        let reclaim = health.idle_reclaim.unwrap();
        let last_result = reclaim.last_result.expect("latest scheduler decision");
        assert_eq!(last_result.status, "skipped");
        assert_eq!(
            last_result.skip_reason.as_deref(),
            Some("min_interval_not_reached")
        );
        let last_attempt = reclaim
            .last_attempt_result
            .expect("last real reclaim attempt remains visible");
        assert_eq!(
            last_attempt.attempted_at_unix_ms,
            first.attempted_at_unix_ms
        );
        assert!(last_attempt.sqlite.attempted);
        assert_eq!(last_attempt.sqlite.success_count, 2);
        assert_eq!(last_attempt.sqlite.failure_count, 0);
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        {
            assert!(last_attempt.allocator.attempted);
            assert_eq!(last_attempt.allocator.success_count, 1);
            assert_eq!(last_attempt.allocator.failure_count, 0);
        }
        #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
        {
            assert!(!last_attempt.allocator.attempted);
            assert_eq!(last_attempt.allocator.status, "unsupported");
        }
        force_idle_reclaim_timeout_elapsed(&state, 120);
        let third = run_idle_reclaim_cycle_if_due(&state).await;

        assert_eq!(third.status, "skipped");
        assert_eq!(
            third.skip_reason.as_deref(),
            Some("min_interval_not_reached")
        );
        assert!(!third.sqlite.attempted);
        assert!(!third.allocator.attempted);
    }

    #[tokio::test]
    async fn idle_reclaim_rechecks_activity_before_invoking_backends() {
        let test_dir = TestDir::new("idle-reclaim-recheck-before-backend");
        let config = idle_reclaim_test_config("http://127.0.0.1:9/v1", 1, 60, true, true);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_reclaim_timeout_elapsed(&state, 120);
        let active_guard: Arc<std::sync::Mutex<Option<ActivityGuard>>> =
            Arc::new(std::sync::Mutex::new(None));
        let active_guard_for_hook = Arc::clone(&active_guard);
        set_idle_reclaim_before_backend_hook(&state, move |state| {
            *active_guard_for_hook.lock().unwrap() = state.idle_reclaim.try_start_http_for_test();
        });

        let result = run_idle_reclaim_cycle_if_due(&state).await;

        assert_eq!(result.status, "skipped");
        assert_eq!(result.skip_reason.as_deref(), Some("active_http_requests"));
        assert!(!result.sqlite.attempted);
        assert!(!result.allocator.attempted);
        drop(active_guard.lock().unwrap().take());
    }

    #[tokio::test]
    async fn idle_reclaim_admission_blocks_http_during_sqlite_backend_window() {
        let test_dir = TestDir::new("idle-reclaim-sqlite-admission");
        let config = idle_reclaim_test_config("http://127.0.0.1:9/v1", 1, 60, true, false);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_reclaim_timeout_elapsed(&state, 120);
        let admission_blocked = Arc::new(AtomicBool::new(false));
        let admission_blocked_for_hook = Arc::clone(&admission_blocked);
        set_idle_reclaim_before_backend_call_hook(&state, move |state| {
            admission_blocked_for_hook.store(
                state.idle_reclaim.try_start_http_for_test().is_none(),
                Ordering::Release,
            );
        });

        let result = run_idle_reclaim_cycle_if_due(&state).await;

        assert!(admission_blocked.load(Ordering::Acquire));
        assert!(result.sqlite.attempted);
        assert!(!result.allocator.attempted);
        assert_eq!(state.idle_reclaim.active_http_requests(), 0);
    }

    #[tokio::test]
    async fn idle_reclaim_admission_blocks_http_during_allocator_backend_window() {
        let test_dir = TestDir::new("idle-reclaim-allocator-admission");
        let config = idle_reclaim_test_config("http://127.0.0.1:9/v1", 1, 60, false, true);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_reclaim_timeout_elapsed(&state, 120);
        let admission_blocked = Arc::new(AtomicBool::new(false));
        let admission_blocked_for_hook = Arc::clone(&admission_blocked);
        set_idle_reclaim_before_backend_call_hook(&state, move |state| {
            admission_blocked_for_hook.store(
                state.idle_reclaim.try_start_http_for_test().is_none(),
                Ordering::Release,
            );
        });

        let result = run_idle_reclaim_cycle_if_due(&state).await;

        assert!(admission_blocked.load(Ordering::Acquire));
        assert!(!result.sqlite.attempted);
        #[cfg(all(target_os = "linux", target_env = "gnu"))]
        assert!(result.allocator.attempted);
        #[cfg(not(all(target_os = "linux", target_env = "gnu")))]
        assert!(!result.allocator.attempted);
        assert_eq!(state.idle_reclaim.active_http_requests(), 0);
    }

    #[tokio::test]
    async fn idle_reclaim_running_cycle_blocks_reentry() {
        let test_dir = TestDir::new("idle-reclaim-no-reentry");
        let config = idle_reclaim_test_config("http://127.0.0.1:9/v1", 1, 60, true, true);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        force_idle_reclaim_timeout_elapsed(&state, 120);
        state.idle_reclaim.running.store(true, Ordering::Release);

        let result = run_idle_reclaim_cycle_if_due(&state).await;

        assert_eq!(result.status, "skipped");
        assert_eq!(result.skip_reason.as_deref(), Some("already_running"));
        assert!(!result.sqlite.attempted);
        assert!(!result.allocator.attempted);
        state.idle_reclaim.running.store(false, Ordering::Release);
    }

    #[tokio::test]
    async fn taken_pipeline_slot_does_not_block_task_status_read() {
        let test_dir = TestDir::new("pipeline-busy-task-status-read");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let task_id = create_persisted_task(
            &state,
            TaskKind::Ask,
            ask_request_metadata("What is cited?", None, None, false, false),
        )
        .await
        .unwrap();
        let pipeline = take_pipeline(&state).expect("take pipeline slot");

        let response = tokio::time::timeout(
            Duration::from_millis(250),
            task_summary_response(&state, task_id.clone()),
        )
        .await
        .expect("task status read does not wait behind pipeline slot")
        .unwrap();

        assert_eq!(response.task.id, task_id);
        restore_pipeline(&state, pipeline).expect("restore pipeline slot");
    }

    async fn assert_exclusive_pipeline_available(state: &SharedState) {
        let deadline = Instant::now() + Duration::from_secs(2);
        loop {
            match with_exclusive_pipeline(state, |_pipeline| Ok::<_, anyhow::Error>(())).await {
                Ok(()) => return,
                Err(error) if is_pipeline_busy_error(&error) && Instant::now() < deadline => {
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
                Err(error) => panic!("exclusive pipeline did not recover: {error}"),
            }
        }
    }

    #[tokio::test]
    async fn cancelled_exclusive_pipeline_owner_restores_slot() {
        let test_dir = TestDir::new("pipeline-owner-cancel-restore");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let (taken_tx, taken_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let worker_state = Arc::clone(&state);
        let owner = tokio::spawn(async move {
            with_exclusive_pipeline(&worker_state, move |_pipeline| {
                taken_tx.send(()).expect("signal pipeline taken");
                release_rx.recv().expect("wait for release signal");
                Ok::<_, anyhow::Error>(())
            })
            .await
        });

        tokio::time::timeout(Duration::from_secs(5), taken_rx)
            .await
            .expect("worker takes pipeline")
            .expect("worker signal remains alive");
        owner.abort();
        release_tx.send(()).expect("release worker");
        let join_error = owner.await.expect_err("owner future was aborted");
        assert!(join_error.is_cancelled());

        assert_exclusive_pipeline_available(&state).await;
    }

    #[tokio::test]
    async fn panicking_exclusive_pipeline_worker_restores_slot() {
        let test_dir = TestDir::new("pipeline-worker-panic-restore");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let result = with_exclusive_pipeline(&state, |_pipeline| -> Result<()> {
            panic!("test panic after taking pipeline")
        })
        .await;

        assert!(result.is_err());
        assert_exclusive_pipeline_available(&state).await;
    }

    #[tokio::test]
    async fn taken_pipeline_slot_serves_cached_index_status() {
        let test_dir = TestDir::new("pipeline-busy-index-status");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let pipeline = take_pipeline(&state).expect("take pipeline slot");

        let Json(response) = tokio::time::timeout(
            Duration::from_millis(250),
            index_status(State(Arc::clone(&state))),
        )
        .await
        .expect("index status read does not wait behind pipeline slot")
        .unwrap();

        assert_eq!(response.source_count, 0);
        assert!(response
            .messages
            .iter()
            .any(|message| message.contains("last-known index status")));
        restore_pipeline(&state, pipeline).expect("restore pipeline slot");
    }

    #[tokio::test]
    async fn taken_pipeline_slot_does_not_block_retrieve_read_snapshot() {
        let test_dir = TestDir::new("pipeline-busy-retrieve-snapshot");
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.embedding.enabled = false;
        config.rerank.enabled = false;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config.clone(), test_dir.path(), pipeline);
        let pipeline = take_pipeline(&state).expect("take pipeline slot");
        let req = RetrieveRequest {
            question: "empty corpus query".into(),
            source_id: None,
            collection_filter: CollectionFilterRequest::default(),
            embedding_profile_id: None,
            limit: None,
            page_size: None,
            page: None,
            fast: false,
            rerank: Some(false),
            dense_top_k: None,
            bm25_top_k: None,
            rerank_top_n: None,
            bypass_cache: false,
            include_debug: true,
            include_debug_packs: false,
            include_locator: false,
            passage: false,
        };
        let controls = resolve_retrieve_controls(&req, &config).unwrap();

        let result = tokio::time::timeout(
            Duration::from_millis(250),
            prepare_retrieve_context(
                Arc::clone(&state),
                &req.question,
                None,
                &config.embedding.profile_id,
                &controls,
            ),
        )
        .await
        .expect("retrieve read snapshot should not wait behind pipeline slot")
        .expect("retrieve read snapshot should not report pipeline busy");

        assert!(result.results.is_empty());
        assert_eq!(
            result.debug.dense_vector_path,
            RetrievalDenseVectorPath::Bm25Only
        );
        restore_pipeline(&state, pipeline).expect("restore pipeline slot");
    }

    #[tokio::test]
    async fn retrieve_read_snapshot_uses_sqlite_reader_resource() {
        let test_dir = TestDir::new("retrieve-uses-sqlite-reader-resource");
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.embedding.enabled = false;
        config.rerank.enabled = false;
        config.daemon.resources.sqlite_reader_concurrency = 1;
        config.daemon.resources.sqlite_reader_queue_capacity = 1;
        config.daemon.resources.sqlite_reader_queue_timeout_seconds = 5;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config.clone(), test_dir.path(), pipeline);
        let held_reader = state
            .resources
            .sqlite_reader
            .acquire()
            .await
            .expect("hold sqlite reader permit");
        let req = RetrieveRequest {
            question: "empty corpus query".into(),
            source_id: None,
            collection_filter: CollectionFilterRequest::default(),
            embedding_profile_id: None,
            limit: None,
            page_size: None,
            page: None,
            fast: false,
            rerank: Some(false),
            dense_top_k: None,
            bm25_top_k: None,
            rerank_top_n: None,
            bypass_cache: false,
            include_debug: true,
            include_debug_packs: false,
            include_locator: false,
            passage: false,
        };
        let controls = resolve_retrieve_controls(&req, &config).unwrap();
        let query_state = Arc::clone(&state);
        let question = req.question.clone();
        let embedding_profile_id = config.embedding.profile_id.clone();
        let retrieve_task = tokio::spawn(async move {
            prepare_retrieve_context(
                query_state,
                &question,
                None,
                &embedding_profile_id,
                &controls,
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let Json(response) = health(State(Arc::clone(&state))).await;
                let reader = response
                    .resources
                    .iter()
                    .find(|resource| resource.name == "sqlite_reader")
                    .expect("sqlite reader resource is reported");
                if reader.active == 1 && reader.queued == 1 {
                    break;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("retrieve is queued behind sqlite reader resource");

        drop(held_reader);
        let result = tokio::time::timeout(Duration::from_secs(2), retrieve_task)
            .await
            .expect("retrieve completes after sqlite reader release")
            .expect("retrieve task joins")
            .expect("retrieve context succeeds");
        assert!(result.results.is_empty());

        let Json(response) = health(State(Arc::clone(&state))).await;
        let reader = response
            .resources
            .iter()
            .find(|resource| resource.name == "sqlite_reader")
            .expect("sqlite reader resource is reported");
        assert_eq!(reader.active, 0);
        assert_eq!(reader.queued, 0);
        assert!(reader.completed >= 2);
    }

    #[tokio::test]
    async fn retrieve_model_wait_does_not_hold_pipeline_slot_when_query_starts_first() {
        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("retrieve-model-wait-releases-pipeline");
        let source_path = test_dir.path().join("doc.md");
        fs::write(
            &source_path,
            "Alpha retrieval evidence keeps the query embedding path active.",
        )
        .unwrap();
        let mut config = retrieve_test_config(&model_server.base_url);
        config.daemon.resources.sqlite_reader_concurrency = 1;
        config.daemon.resources.sqlite_reader_queue_capacity = 1;
        config.daemon.resources.sqlite_reader_queue_timeout_seconds = 5;
        let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        pipeline.ingest_source(&source_id).await.unwrap();
        let state = test_state(config.clone(), test_dir.path(), pipeline);
        let task_id = create_persisted_task(
            &state,
            TaskKind::Ask,
            ask_request_metadata("What is cited?", None, None, false, false),
        )
        .await
        .unwrap();
        let before_query_embeddings = model_server.embedding_requests();
        model_server.block_embeddings();

        let controls = resolve_retrieve_controls(
            &RetrieveRequest {
                question: "Alpha retrieval question?".into(),
                source_id: Some(source_id.0.clone()),
                collection_filter: CollectionFilterRequest::default(),
                embedding_profile_id: None,
                limit: Some(3),
                page_size: Some(1),
                page: Some(1),
                fast: false,
                rerank: Some(false),
                dense_top_k: None,
                bm25_top_k: None,
                rerank_top_n: None,
                bypass_cache: false,
                include_debug: true,
                include_debug_packs: false,
                include_locator: false,
                passage: false,
            },
            &config,
        )
        .unwrap();
        let query_state = Arc::clone(&state);
        let embedding_profile_id = config.embedding.profile_id.clone();
        let source_filter = HashSet::from([source_id.clone()]);
        let retrieve_task = tokio::spawn(async move {
            prepare_retrieve_context(
                query_state,
                "Alpha retrieval question?",
                Some(source_filter),
                &embedding_profile_id,
                &controls,
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(2), async {
            while model_server.embedding_requests() <= before_query_embeddings {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("retrieve reached blocked embedding provider");

        assert_exclusive_pipeline_available(&state).await;
        let blocked_reader = state.resources.sqlite_reader.snapshot();
        assert_eq!(
            blocked_reader.active, 0,
            "blocked embedding wait must not hold sqlite_reader"
        );
        assert_eq!(
            blocked_reader.queued, 0,
            "blocked embedding wait must not queue unrelated sqlite_reader work"
        );

        let response = tokio::time::timeout(
            Duration::from_millis(250),
            task_summary_response(&state, task_id.clone()),
        )
        .await
        .expect("task status read remains responsive while embedding is blocked")
        .unwrap();
        assert_eq!(response.task.id, task_id);
        let after_status_reader = state.resources.sqlite_reader.snapshot();
        assert!(
            after_status_reader.completed > blocked_reader.completed,
            "task status read should be counted by sqlite_reader"
        );
        assert_eq!(after_status_reader.active, 0);

        model_server.release_embeddings();
        let context = tokio::time::timeout(Duration::from_secs(5), retrieve_task)
            .await
            .expect("retrieve completes after embedding release")
            .expect("retrieve task joins")
            .expect("retrieve context succeeds");
        assert!(!context.results.is_empty());
        let completed_reader = state.resources.sqlite_reader.snapshot();
        assert!(
            completed_reader.completed > after_status_reader.completed,
            "retrieve SQLite snapshot reads after provider release should be counted by sqlite_reader"
        );
    }

    #[tokio::test]
    async fn query_capability_wait_does_not_hold_pipeline_slot_when_query_starts_first() {
        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("query-capability-wait-releases-pipeline");
        let config = retrieve_test_config(&model_server.base_url);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config.clone(), test_dir.path(), pipeline);
        let before_model_requests = model_server.model_requests();
        model_server.block_models();

        let refresh_state = Arc::clone(&state);
        let embedding_profile_id = config.embedding.profile_id.clone();
        let refresh_task = tokio::spawn(async move {
            refresh_query_embedding_profile_capabilities(
                &refresh_state,
                true,
                &embedding_profile_id,
            )
            .await
        });

        tokio::time::timeout(Duration::from_secs(2), async {
            while model_server.model_requests() <= before_model_requests {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("query refresh reached blocked capability discovery");

        assert_exclusive_pipeline_available(&state).await;

        model_server.release_models();
        tokio::time::timeout(Duration::from_secs(5), refresh_task)
            .await
            .expect("query capability refresh completes after discovery release")
            .expect("query capability refresh task joins")
            .expect("query capability refresh succeeds");
    }

    #[tokio::test]
    async fn index_status_capability_wait_does_not_hold_pipeline_slot() {
        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("index-status-capability-wait-releases-pipeline");
        let config = retrieve_test_config(&model_server.base_url);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let before_model_requests = model_server.model_requests();
        model_server.block_models();

        let status_state = Arc::clone(&state);
        let status_task = tokio::spawn(async move { index_status(State(status_state)).await });

        tokio::time::timeout(Duration::from_secs(2), async {
            while model_server.model_requests() <= before_model_requests {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("index status reached blocked capability discovery");

        assert_exclusive_pipeline_available(&state).await;

        model_server.release_models();
        let _response = tokio::time::timeout(Duration::from_secs(5), status_task)
            .await
            .expect("index status completes after discovery release")
            .expect("index status task joins")
            .expect("index status succeeds");
    }

    #[tokio::test]
    async fn check_stale_capability_wait_does_not_hold_pipeline_slot() {
        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("check-stale-capability-wait-releases-pipeline");
        let config = retrieve_test_config(&model_server.base_url);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let before_model_requests = model_server.model_requests();
        model_server.block_models();

        let check_state = Arc::clone(&state);
        let check_task = tokio::spawn(async move { check_stale(State(check_state)).await });

        tokio::time::timeout(Duration::from_secs(2), async {
            while model_server.model_requests() <= before_model_requests {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("check stale reached blocked capability discovery");

        assert_exclusive_pipeline_available(&state).await;

        model_server.release_models();
        let _response = tokio::time::timeout(Duration::from_secs(5), check_task)
            .await
            .expect("check stale completes after discovery release")
            .expect("check stale task joins")
            .expect("check stale succeeds");
    }

    #[tokio::test]
    async fn batch_source_selection_capability_wait_does_not_hold_pipeline_slot() {
        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("batch-selection-capability-wait-releases-pipeline");
        let config = retrieve_test_config(&model_server.base_url);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let before_model_requests = model_server.model_requests();
        model_server.block_models();

        let select_state = Arc::clone(&state);
        let select_task = tokio::spawn(async move {
            background_ingest_batch_sources(&select_state, false, false).await
        });

        tokio::time::timeout(Duration::from_secs(2), async {
            while model_server.model_requests() <= before_model_requests {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("batch source selection reached blocked capability discovery");

        assert_exclusive_pipeline_available(&state).await;

        model_server.release_models();
        tokio::time::timeout(Duration::from_secs(5), select_task)
            .await
            .expect("batch source selection completes after discovery release")
            .expect("batch source selection task joins")
            .expect("batch source selection succeeds");
    }

    #[tokio::test]
    async fn task_list_all_clamps_large_limit_and_reports_total() {
        let test_dir = TestDir::new("task-list-all-clamped");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        {
            let store = state.task_store.lock().unwrap();
            for index in 0..TASK_LIST_MAX_LIMIT + 5 {
                let task_id = TaskId(format!("task-history-{index:03}"));
                store
                    .create_task(
                        &task_id,
                        TaskKind::Ask,
                        &ask_request_metadata("What is cited?", None, None, false, false),
                    )
                    .unwrap();
                store.start_task(&task_id).unwrap();
                store
                    .finish_task_success(&task_id, &ask_result_metadata("answer", 0, false, false))
                    .unwrap();
            }
        }

        let Json(response) = list_tasks_handler(
            State(Arc::clone(&state)),
            Query(TaskListQuery {
                status: Some("all".into()),
                limit: Some(usize::MAX),
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.total, TASK_LIST_MAX_LIMIT + 5);
        assert_eq!(response.tasks.len(), TASK_LIST_MAX_LIMIT);
        assert!(response
            .tasks
            .iter()
            .all(|task| task.status == TaskStatus::Succeeded));
    }

    #[tokio::test]
    async fn task_queue_plateau_exposes_completed_work_and_backfill() {
        let test_dir = TestDir::new("task-queue-plateau");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let first_active = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(
                Some("src-active-1"),
                false,
                None,
                false,
                true,
            ),
        )
        .await
        .unwrap();
        let second_active = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(
                Some("src-active-2"),
                false,
                None,
                false,
                true,
            ),
        )
        .await
        .unwrap();
        let completed = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(
                Some("src-done"),
                false,
                None,
                false,
                true,
            ),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &completed).await.unwrap();
        finish_task_success(
            &state,
            &completed,
            ingest_result_metadata(1, &EmbeddingCacheStats::default()),
        )
        .await
        .unwrap();

        let Json(response) = list_tasks_handler(
            State(Arc::clone(&state)),
            Query(TaskListQuery {
                status: Some("active".into()),
                limit: Some(20),
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.total, 2);
        assert!(response.tasks.iter().any(|task| task.id == first_active));
        assert!(response.tasks.iter().any(|task| task.id == second_active));
        let aggregate = response.aggregate.expect("aggregate metadata is present");
        assert_eq!(aggregate.active_total, 2);
        assert_eq!(aggregate.turnover.recent_terminalized, 1);
        assert_eq!(aggregate.turnover.recent_succeeded, 1);
        assert_eq!(aggregate.turnover.recent_backfilled, 2);
        assert!(aggregate.turnover.window.event_sequence_ceiling >= 4);
    }

    #[tokio::test]
    async fn publish_complete_running_tasks_expose_stale_reason() {
        let test_dir = TestDir::new("publish-complete-running-tasks");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let task_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(
                Some("src-publish"),
                false,
                None,
                false,
                true,
            ),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &task_id).await.unwrap();
        record_task_progress(
            &state,
            &task_id,
            TaskProgressSnapshot::phase(IngestTaskStage::VectorIndex.as_str())
                .with_counter("sources", 1, Some(1))
                .with_wait_reason("post_publish_cleanup")
                .with_recent_status("index publishing complete"),
        )
        .await;

        let Json(response) = list_tasks_handler(
            State(Arc::clone(&state)),
            Query(TaskListQuery {
                status: Some("active".into()),
                limit: Some(20),
            }),
        )
        .await
        .unwrap();

        let aggregate = response.aggregate.expect("aggregate metadata is present");
        assert_eq!(aggregate.stale_running.publish_complete_running, 1);
        assert_eq!(
            aggregate.stale_running.reason_buckets[0].reason,
            "post_publish_cleanup"
        );
        let detail = task_summary_response(&state, task_id).await.unwrap().task;
        assert_eq!(
            detail.progress.and_then(|progress| progress.wait_reason),
            Some("post_publish_cleanup".into())
        );
    }

    #[tokio::test]
    async fn embedding_wait_reason_metadata_exposes_counts_age_and_buckets() {
        let test_dir = TestDir::new("embedding-wait-reason-metadata");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        for (source_id, reason) in [
            ("src-batch-wait", "embedding_batch"),
            ("src-throughput-wait", "embedding_throughput"),
        ] {
            let task_id = create_persisted_task(
                &state,
                TaskKind::Ingest,
                ingest_task_request_metadata_with_queue_claim(
                    Some(source_id),
                    false,
                    None,
                    false,
                    true,
                ),
            )
            .await
            .unwrap();
            ensure_task_started(&state, &task_id).await.unwrap();
            let mut progress =
                TaskProgressSnapshot::phase(IngestTaskStage::EmbeddingQueueWait.as_str())
                    .with_counter("embedding_vectors", 0, Some(10))
                    .with_wait_reason(reason)
                    .with_recent_status("waiting for embedding batch");
            if let Some(phase) = &mut progress.phase {
                phase.started_at = "1".into();
            }
            record_task_progress(&state, &task_id, progress).await;
        }

        let Json(response) = list_tasks_handler(
            State(Arc::clone(&state)),
            Query(TaskListQuery {
                status: Some("active".into()),
                limit: Some(20),
            }),
        )
        .await
        .unwrap();

        let aggregate = response.aggregate.expect("aggregate metadata is present");
        assert_eq!(aggregate.embedding_wait.waiting, 2);
        assert!(aggregate.embedding_wait.oldest_wait_ms.unwrap_or_default() > 60_000);
        assert!(aggregate
            .embedding_wait
            .reason_buckets
            .iter()
            .any(|bucket| bucket.reason == "embedding_batch" && bucket.count == 1));
        assert!(aggregate
            .embedding_wait
            .reason_buckets
            .iter()
            .any(|bucket| bucket.reason == "embedding_throughput" && bucket.count == 1));
        assert!(response.tasks.iter().all(|task| {
            task.progress
                .as_ref()
                .and_then(|progress| progress.wait_reason.as_ref())
                .is_some()
        }));
    }

    #[tokio::test]
    async fn task_list_aggregate_metadata_counts_waits_beyond_returned_sample() {
        let test_dir = TestDir::new("task-list-aggregate-beyond-sample");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        for index in 0..(TASK_QUEUE_AGGREGATE_ACTIVE_SAMPLE_LIMIT + 5) {
            create_persisted_task_with_id(
                &state,
                TaskId(format!("task-aggregate-filler-{index:03}")),
                TaskKind::Ask,
                ask_request_metadata(
                    &format!("queued filler question {index}"),
                    None,
                    None,
                    false,
                    false,
                ),
            )
            .await
            .unwrap();
        }

        let embedding_wait = create_persisted_task_with_id(
            &state,
            TaskId("task-zz-aggregate-embedding-wait".into()),
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(
                Some("src-wait-beyond-sample"),
                false,
                None,
                false,
                true,
            ),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &embedding_wait).await.unwrap();
        let mut progress =
            TaskProgressSnapshot::phase(IngestTaskStage::EmbeddingQueueWait.as_str())
                .with_counter("embedding_vectors", 0, Some(10))
                .with_wait_reason("embedding_throughput")
                .with_recent_status("waiting for embedding model throughput");
        if let Some(phase) = &mut progress.phase {
            phase.started_at = "1".into();
        }
        record_task_progress(&state, &embedding_wait, progress).await;

        let publish_wait = create_persisted_task_with_id(
            &state,
            TaskId("task-zz-aggregate-publish-wait".into()),
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(
                Some("src-publish-beyond-sample"),
                false,
                None,
                false,
                true,
            ),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &publish_wait).await.unwrap();
        record_task_progress(
            &state,
            &publish_wait,
            TaskProgressSnapshot::phase(IngestTaskStage::VectorIndex.as_str())
                .with_counter("sources", 1, Some(1))
                .with_wait_reason("post_publish_cleanup")
                .with_recent_status("index publishing complete"),
        )
        .await;

        let Json(response) = list_tasks_handler(
            State(Arc::clone(&state)),
            Query(TaskListQuery {
                status: Some("active".into()),
                limit: Some(20),
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.total, TASK_QUEUE_AGGREGATE_ACTIVE_SAMPLE_LIMIT + 7);
        assert!(!response.tasks.iter().any(|task| task.id == embedding_wait));
        assert!(!response.tasks.iter().any(|task| task.id == publish_wait));
        let aggregate = response.aggregate.expect("aggregate metadata is present");
        assert_eq!(aggregate.embedding_wait.waiting, 1);
        assert!(aggregate.embedding_wait.oldest_wait_ms.unwrap_or_default() > 60_000);
        assert_eq!(
            aggregate.embedding_wait.reason_buckets[0].reason,
            "embedding_throughput"
        );
        assert_eq!(aggregate.stale_running.publish_complete_running, 1);
        assert_eq!(
            aggregate.stale_running.reason_buckets[0].reason,
            "post_publish_cleanup"
        );
    }

    #[tokio::test]
    async fn active_queue_turnover_metadata_reports_bounded_event_window() {
        let test_dir = TestDir::new("active-queue-turnover-metadata");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let active = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(
                Some("src-backfilled"),
                false,
                None,
                false,
                true,
            ),
        )
        .await
        .unwrap();
        let completed = create_persisted_task(
            &state,
            TaskKind::Ask,
            ask_request_metadata("What is cited?", None, None, false, false),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &completed).await.unwrap();
        finish_task_success(
            &state,
            &completed,
            ask_result_metadata("answer", 0, false, false),
        )
        .await
        .unwrap();

        let Json(response) = list_tasks_handler(
            State(Arc::clone(&state)),
            Query(TaskListQuery {
                status: Some("active".into()),
                limit: Some(1),
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.total, 1);
        assert_eq!(response.tasks[0].id, active);
        let aggregate = response.aggregate.expect("aggregate metadata is present");
        assert_eq!(
            aggregate.turnover.window.event_limit,
            TASK_QUEUE_TURNOVER_EVENT_LIMIT
        );
        assert!(
            aggregate.turnover.window.event_sequence_floor
                <= aggregate.turnover.window.event_sequence_ceiling
        );
        assert_eq!(aggregate.turnover.recent_terminalized, 1);
        assert_eq!(aggregate.turnover.recent_backfilled, 1);
    }

    #[tokio::test]
    async fn plateau_queue_integration_daemon_api_metadata_is_wire_serializable() {
        let test_dir = TestDir::new("plateau-queue-integration-daemon-api");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let embedding_wait = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(
                Some("src-embedding-wait"),
                false,
                None,
                false,
                true,
            ),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &embedding_wait).await.unwrap();
        record_task_progress(
            &state,
            &embedding_wait,
            TaskProgressSnapshot::phase(IngestTaskStage::EmbeddingQueueWait.as_str())
                .with_counter("embedding_vectors", 0, Some(10))
                .with_wait_reason("embedding_batch")
                .with_recent_status("waiting for embedding batch"),
        )
        .await;

        let publish_wait = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(
                Some("src-publish-wait"),
                false,
                None,
                false,
                true,
            ),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &publish_wait).await.unwrap();
        record_task_progress(
            &state,
            &publish_wait,
            TaskProgressSnapshot::phase(IngestTaskStage::VectorIndex.as_str())
                .with_counter("sources", 1, Some(1))
                .with_wait_reason("post_publish_cleanup")
                .with_recent_status("index publishing complete"),
        )
        .await;

        let completed = create_persisted_task(
            &state,
            TaskKind::Ask,
            ask_request_metadata("What is cited?", None, None, false, false),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &completed).await.unwrap();
        finish_task_success(
            &state,
            &completed,
            ask_result_metadata("answer", 0, false, false),
        )
        .await
        .unwrap();

        let Json(response) = list_tasks_handler(
            State(Arc::clone(&state)),
            Query(TaskListQuery {
                status: Some("active".into()),
                limit: Some(20),
            }),
        )
        .await
        .unwrap();
        let wire_json = serde_json::to_string(&response).unwrap();
        let decoded: TaskListResponse = serde_json::from_str(&wire_json).unwrap();

        assert_eq!(decoded.total, 2);
        let aggregate = decoded.aggregate.expect("aggregate metadata is present");
        assert_eq!(aggregate.active_total, 2);
        assert_eq!(aggregate.turnover.recent_terminalized, 1);
        assert_eq!(aggregate.turnover.recent_backfilled, 2);
        assert_eq!(aggregate.embedding_wait.waiting, 1);
        assert_eq!(
            aggregate.embedding_wait.reason_buckets[0].reason,
            "embedding_batch"
        );
        assert_eq!(aggregate.stale_running.publish_complete_running, 1);
        assert_eq!(
            aggregate.stale_running.reason_buckets[0].reason,
            "post_publish_cleanup"
        );
    }

    #[test]
    fn ask_request_defaults_retrieval_debug_off() {
        let req: AskRequest =
            serde_json::from_value(serde_json::json!({"question": "What is cited?"})).unwrap();

        assert_eq!(req.question, "What is cited?");
        assert!(req.source_id.is_none());
        assert!(req.collection_filter.is_empty());
        assert!(!req.show_retrieval);
        assert!(!req.context_only);
        assert!(req.limit.is_none());
        assert!(req.page_size.is_none());
        assert!(req.page.is_none());
    }

    #[test]
    fn context_only_ask_request_plumbs_retrieve_pagination_controls() {
        let retrieve_req = context_only_retrieve_request(AskRequest {
            question: "What is cited?".into(),
            source_id: Some("src-1".into()),
            collection_filter: CollectionFilterRequest::default(),
            embedding_profile_id: Some("alt".into()),
            show_retrieval: true,
            context_only: true,
            limit: Some(7),
            page_size: Some(2),
            page: Some(3),
        });

        assert_eq!(retrieve_req.question, "What is cited?");
        assert_eq!(retrieve_req.source_id.as_deref(), Some("src-1"));
        assert_eq!(retrieve_req.embedding_profile_id.as_deref(), Some("alt"));
        assert_eq!(retrieve_req.limit, Some(7));
        assert_eq!(retrieve_req.page_size, Some(2));
        assert_eq!(retrieve_req.page, Some(3));
        assert!(retrieve_req.include_debug);
        assert!(retrieve_req.include_locator);
    }

    #[test]
    fn generated_ask_request_rejects_retrieve_pagination_controls() {
        let error = validate_ask_retrieve_controls(&AskRequest {
            question: "What is cited?".into(),
            source_id: None,
            collection_filter: CollectionFilterRequest::default(),
            embedding_profile_id: None,
            show_retrieval: false,
            context_only: false,
            limit: None,
            page_size: None,
            page: Some(2),
        })
        .unwrap_err();

        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        assert!(error.1.error.contains("context_only"));
    }

    include!("tests/ask_response_serialization_tests.rs");

    #[test]
    fn retrieve_response_pages_context_pack_without_full_locator_by_default() {
        let results = vec![
            test_retrieval_result(1, "chunk-1", "ev-1", EvidenceKind::Text),
            test_retrieval_result(2, "chunk-2", "ev-2", EvidenceKind::Text),
        ];
        let mut debug = empty_retrieval_debug();
        refresh_final_evidence_pack_debug(&mut debug, &results);

        let response = persisted_retrieve_response(RetrieveResponseInput {
            task_id: TaskId("task-1".into()),
            query: "What is cited?".into(),
            source_filter: Some(SourceId("src".into())),
            collection_filter: None,
            collection_provenance: HashMap::new(),
            embedding_profile_id: EmbeddingProfileId::default_profile(),
            controls: EffectiveRetrieveControls {
                limit: 2,
                page_size: 1,
                page: 2,
                include_debug: false,
                include_debug_packs: false,
                include_locator: false,
                passage: false,
                bypass_cache: false,
                fast: false,
                config: Config::default(),
                retrieval_config: RetrievalConfig::default(),
                rerank_config: RerankConfig::default(),
            },
            results,
            debug,
            source_paths: HashMap::new(),
            retrieval_ms: 7,
        });

        assert_eq!(response.total_results, 2);
        assert_eq!(response.returned_results, 1);
        assert_eq!(response.results[0].index, 1);
        assert_eq!(response.results[0].evidence_id, "ev-2");
        assert!(response.results[0].structured_locator.is_none());
        assert!(response.results[0].provenance.is_none());
        assert!(response.debug.is_none());
    }

    #[test]
    fn retrieve_response_no_passage_uses_canonical_display_support_pack() {
        let results = vec![test_canonical_retrieval_result(
            1,
            "chunk-2tim4",
            &[("ev-1", 1), ("ev-8", 8), ("ev-9", 9)],
        )];
        let mut debug = empty_retrieval_debug();
        refresh_final_evidence_pack_debug(&mut debug, &results);
        debug.display_evidence_pack = vec![RetrievalEvidencePackEntry {
            label: "E1".into(),
            ..debug.final_evidence_pack[1].clone()
        }];
        debug.display_evidence_count = debug.display_evidence_pack.len();

        let response = persisted_retrieve_response(RetrieveResponseInput {
            task_id: TaskId("task-1".into()),
            query: "crown of righteousness".into(),
            source_filter: Some(SourceId("src".into())),
            collection_filter: None,
            collection_provenance: HashMap::new(),
            embedding_profile_id: EmbeddingProfileId::default_profile(),
            controls: EffectiveRetrieveControls {
                limit: 1,
                page_size: 1,
                page: 1,
                include_debug: false,
                include_debug_packs: false,
                include_locator: true,
                passage: false,
                bypass_cache: false,
                fast: false,
                config: Config::default(),
                retrieval_config: RetrievalConfig::default(),
                rerank_config: RerankConfig::default(),
            },
            results,
            debug,
            source_paths: HashMap::new(),
            retrieval_ms: 7,
        });

        assert_eq!(response.total_results, 1);
        assert_eq!(response.returned_results, 1);
        let result = &response.results[0];
        assert_eq!(result.evidence_id, "ev-8");
        assert_eq!(result.chunk_id, "chunk-2tim4");
        assert_eq!(result.score, 1.0);
        assert_eq!(result.locator, "2 Timothy 4:8");
        assert_eq!(result.snippet, "verse 8 text.");
        assert!(matches!(
            result.structured_locator,
            Some(SourceLocator::Canonical { .. })
        ));
    }

    #[test]
    fn retrieve_response_passage_mode_pages_by_canonical_chunk() {
        let results = vec![test_canonical_retrieval_result(
            1,
            "chunk-2tim4",
            &[("ev-1", 1), ("ev-2", 2), ("ev-3", 3)],
        )];
        let mut debug = empty_retrieval_debug();
        refresh_final_evidence_pack_debug(&mut debug, &results);
        debug.display_evidence_pack = vec![RetrievalEvidencePackEntry {
            label: "E1".into(),
            ..debug.final_evidence_pack[1].clone()
        }];
        debug.display_evidence_count = debug.display_evidence_pack.len();

        let response = persisted_retrieve_response(RetrieveResponseInput {
            task_id: TaskId("task-1".into()),
            query: "crown of righteousness".into(),
            source_filter: Some(SourceId("src".into())),
            collection_filter: None,
            collection_provenance: HashMap::new(),
            embedding_profile_id: EmbeddingProfileId::default_profile(),
            controls: EffectiveRetrieveControls {
                limit: 1,
                page_size: 1,
                page: 1,
                include_debug: false,
                include_debug_packs: false,
                include_locator: true,
                passage: true,
                bypass_cache: false,
                fast: false,
                config: Config::default(),
                retrieval_config: RetrievalConfig::default(),
                rerank_config: RerankConfig::default(),
            },
            results,
            debug,
            source_paths: HashMap::new(),
            retrieval_ms: 7,
        });

        assert_eq!(response.total_results, 1);
        assert_eq!(response.returned_results, 1);
        assert_eq!(response.results.len(), 1);
        let passage = &response.results[0];
        assert_eq!(passage.index, 0);
        assert_eq!(passage.rank, 1);
        assert_eq!(passage.locator, "2 Timothy 4:1-3");
        assert_eq!(passage.snippet, "verse 1 text. verse 2 text. verse 3 text.");
        assert!(matches!(
            passage.structured_locator,
            Some(SourceLocator::Canonical { .. })
        ));
    }

    #[test]
    fn retrieve_response_passage_mode_uses_ranked_chunk_membership_without_debug_pack() {
        let results = vec![
            test_canonical_retrieval_result(
                1,
                "chunk-2tim4",
                &[("ev-1", 1), ("ev-2", 2), ("ev-3", 3)],
            ),
            test_canonical_retrieval_result(
                2,
                "chunk-ps23",
                &[("ev-ps23-1", 1), ("ev-ps23-2", 2), ("ev-ps23-3", 3)],
            ),
        ];
        let mut debug = empty_retrieval_debug();
        debug.evidence_pack_mode = RetrievalDebugEvidencePackMode::Compact;
        debug.final_evidence_pack.clear();
        debug.display_evidence_pack.clear();

        let response = persisted_retrieve_response(RetrieveResponseInput {
            task_id: TaskId("task-1".into()),
            query: "crown of righteousness".into(),
            source_filter: Some(SourceId("src".into())),
            collection_filter: None,
            collection_provenance: HashMap::new(),
            embedding_profile_id: EmbeddingProfileId::default_profile(),
            controls: EffectiveRetrieveControls {
                limit: 1,
                page_size: 1,
                page: 1,
                include_debug: false,
                include_debug_packs: false,
                include_locator: true,
                passage: true,
                bypass_cache: false,
                fast: false,
                config: Config::default(),
                retrieval_config: RetrievalConfig::default(),
                rerank_config: RerankConfig::default(),
            },
            results,
            debug,
            source_paths: HashMap::new(),
            retrieval_ms: 7,
        });

        assert_eq!(response.total_results, 2);
        assert_eq!(response.returned_results, 1);
        let passage = &response.results[0];
        assert_eq!(passage.evidence_id, "ev-1");
        assert_eq!(passage.chunk_id, "chunk-2tim4");
        assert_eq!(passage.locator, "2 Timothy 4:1-3");
        assert_eq!(passage.snippet, "verse 1 text. verse 2 text. verse 3 text.");
        assert!(matches!(
            passage.structured_locator,
            Some(SourceLocator::Canonical { .. })
        ));
    }

    #[test]
    fn retrieve_debug_options_passage_default_skips_full_pack_and_support_selection() {
        let controls = EffectiveRetrieveControls {
            limit: 1,
            page_size: 10,
            page: 1,
            include_debug: false,
            include_debug_packs: false,
            include_locator: false,
            passage: true,
            bypass_cache: false,
            fast: false,
            config: Config::default(),
            retrieval_config: RetrievalConfig::default(),
            rerank_config: RerankConfig::default(),
        };

        let options = retrieve_debug_options(&controls);

        assert_eq!(
            options.evidence_pack_mode,
            RetrievalDebugEvidencePackMode::Compact
        );
        assert_eq!(
            options.canonical_budget.support,
            RetrievalDisplayScope::Window { start: 0, len: 0 }
        );
        assert_eq!(
            options.canonical_budget.display,
            RetrievalDisplayScope::Window { start: 0, len: 0 }
        );
    }

    include!("ask_debug_selection_tests.rs");

    #[tokio::test]
    async fn delete_source_missing_returns_not_found() {
        let test_dir = TestDir::new("delete-missing-source");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let (status, Json(body)) = delete_source(
            State(state),
            Path("__missing_source_smoke_retest__".to_string()),
        )
        .await
        .unwrap_err();

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body.error.as_ref(),
            "source not found: __missing_source_smoke_retest__"
        );
    }

    #[tokio::test]
    async fn add_source_missing_path_does_not_wait_for_sqlite_writer() {
        let test_dir = TestDir::new("add-source-no-writer-wait");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let writer_permit = state
            .resources
            .sqlite_writer
            .acquire()
            .await
            .expect("hold sqlite writer permit");

        let (status, Json(body)) = tokio::time::timeout(
            Duration::from_millis(250),
            add_source(
                State(Arc::clone(&state)),
                Json(AddSourceRequest {
                    path: test_dir.path().join("missing.md").display().to_string(),
                }),
            ),
        )
        .await
        .expect("add_source validates missing path before writer admission")
        .unwrap_err();

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.error.contains("resolve path"));
        drop(writer_permit);
    }

    #[tokio::test]
    async fn delete_source_existing_removes_source() {
        let test_dir = TestDir::new("delete-existing-source");
        let source_path = test_dir.path().join("doc.md");
        fs::write(&source_path, "delete me").unwrap();
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let response = delete_source(State(Arc::clone(&state)), Path(source_id.0.clone()))
            .await
            .unwrap();

        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let pipeline = state.pipeline.lock().unwrap();
        let pipeline = pipeline.as_ref().unwrap();
        assert!(pipeline.store().get_source(&source_id).unwrap().is_none());
    }

    include!("tests/deletion_lifecycle_tests.rs");

    #[tokio::test]
    async fn delete_source_publish_wait_serves_cached_index_status() {
        let test_dir = TestDir::new("delete-source-publish-wait-status");
        let source_path = test_dir.path().join("doc.md");
        fs::write(&source_path, "delete me while publish waits").unwrap();
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let publish_permit = state
            .resources
            .index_publish
            .acquire()
            .await
            .expect("hold index publish permit");
        let delete_state = Arc::clone(&state);
        let delete_source_id = source_id.clone();
        let delete_task = tokio::spawn(async move {
            delete_source(State(delete_state), Path(delete_source_id.0)).await
        });

        let deadline = Instant::now() + Duration::from_secs(1);
        loop {
            let slot_taken = state.pipeline.lock().unwrap().is_none();
            if slot_taken {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "delete_source did not take the pipeline slot"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
        }

        let Json(response) = tokio::time::timeout(
            Duration::from_millis(250),
            index_status(State(Arc::clone(&state))),
        )
        .await
        .expect("index status falls back while delete_source waits on publish")
        .unwrap();
        assert!(response
            .messages
            .iter()
            .any(|message| message.contains("last-known index status")));

        drop(publish_permit);
        let response = tokio::time::timeout(Duration::from_secs(5), delete_task)
            .await
            .expect("delete_source completes after publish permit release")
            .expect("delete task joins")
            .unwrap();
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        let pipeline = state.pipeline.lock().unwrap();
        let pipeline = pipeline.as_ref().unwrap();
        assert!(pipeline.store().get_source(&source_id).unwrap().is_none());
    }

    #[tokio::test]
    async fn sync_collection_missing_collection_does_not_wait_for_sqlite_writer() {
        let test_dir = TestDir::new("sync-collection-no-writer-wait");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let writer_permit = state
            .resources
            .sqlite_writer
            .acquire()
            .await
            .expect("hold sqlite writer permit");

        let (status, Json(body)) = tokio::time::timeout(
            Duration::from_millis(250),
            sync_collection(
                State(Arc::clone(&state)),
                Path("missing".into()),
                Json(CollectionSyncRequest {
                    paths: Vec::new(),
                    max_depth: None,
                }),
            ),
        )
        .await
        .expect("sync_collection validates missing collection before writer admission")
        .unwrap_err();

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body.error.as_ref(), "collection not found: missing");
        drop(writer_permit);
    }

    #[tokio::test]
    async fn collection_handlers_create_root_sync_and_status() {
        let test_dir = TestDir::new("collection-sync");
        let root = test_dir.path().join("articles");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("one.md"), "one").unwrap();
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let (status, Json(created)) = create_collection(
            State(Arc::clone(&state)),
            Json(CreateCollectionRequest {
                name: "articles".into(),
                ignore_patterns: Vec::new(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(created.collection.name, "articles");

        let Json(with_root) = add_collection_root(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(AddCollectionRootRequest {
                path: root.display().to_string(),
            }),
        )
        .await
        .unwrap();
        assert!(with_root.added);
        assert_eq!(with_root.root_count, 1);
        assert_eq!(with_root.member_count, 0);

        let Json(sync) = sync_collection(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionSyncRequest {
                paths: Vec::new(),
                max_depth: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(sync.report.member_count, 1);
        assert_eq!(sync.report.added, 1);

        let Json(collection) = get_collection(State(Arc::clone(&state)), Path("articles".into()))
            .await
            .unwrap();
        assert_eq!(collection.members.len(), 1);
        assert_eq!(collection.members[0].logical_path, "one.md");

        let Json(status) = collection_status(State(Arc::clone(&state)), Path("articles".into()))
            .await
            .unwrap();
        assert_eq!(status.status.member_count, 1);
        assert_eq!(status.status.root_count, 1);
    }

    #[tokio::test]
    async fn add_collection_root_existing_response_is_compact_with_many_members() {
        let test_dir = TestDir::new("collection-root-compact");
        let root = test_dir.path().join("articles");
        fs::create_dir_all(&root).unwrap();
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let (status, Json(created)) = create_collection(
            State(Arc::clone(&state)),
            Json(CreateCollectionRequest {
                name: "articles".into(),
                ignore_patterns: Vec::new(),
            }),
        )
        .await
        .unwrap();
        assert_eq!(status, StatusCode::CREATED);
        assert_eq!(created.collection.name, "articles");

        let Json(first_add) = add_collection_root(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(AddCollectionRootRequest {
                path: root.display().to_string(),
            }),
        )
        .await
        .unwrap();
        assert!(first_add.added);
        assert_eq!(first_add.root_count, 1);
        assert_eq!(first_add.member_count, 0);

        let member_root = root.clone();
        with_task_store_write(&state, move |store| {
            let candidates = (0..128)
                .map(|index| {
                    let source_id = SourceId(format!("src-{index}"));
                    let source_path = member_root.join(format!("member-{index}.md"));
                    store.add_source(&verbatim_core::types::Source {
                        id: source_id.clone(),
                        path: source_path.clone(),
                        hash: format!("hash-{index}"),
                        status: SourceStatus::Pending,
                        parser_used: None,
                        last_ingested_at: None,
                    })?;
                    Ok(CollectionMemberCandidate {
                        source_id,
                        logical_path: format!("member-{index}.md"),
                        source_path,
                    })
                })
                .collect::<Result<Vec<_>>>()?;
            store.replace_collection_members(
                "articles",
                &candidates,
                verbatim_core::collection::CollectionSyncReport {
                    member_count: 0,
                    added: 0,
                    removed: 0,
                    unchanged: 0,
                    scanned_roots: 1,
                    max_depth: 32,
                    skipped: Vec::new(),
                },
            )?;
            Ok(())
        })
        .await
        .unwrap();

        let Json(existing_add) = add_collection_root(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(AddCollectionRootRequest {
                path: root.display().to_string(),
            }),
        )
        .await
        .unwrap();

        assert!(!existing_add.added);
        assert_eq!(existing_add.collection_name, "articles");
        assert_eq!(existing_add.root.path, root);
        assert_eq!(existing_add.root.kind.as_str(), "directory");
        assert_eq!(existing_add.root_count, 1);
        assert_eq!(existing_add.member_count, 128);
        let wire = serde_json::to_string(&existing_add).unwrap();
        assert!(!wire.contains("\"members\":["));
        assert!(!wire.contains("member-127.md"));
    }

    #[tokio::test]
    async fn collection_watcher_api_persists_settings_and_reports_status() {
        let test_dir = TestDir::new("collection-watcher-api");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let _ = create_collection(
            State(Arc::clone(&state)),
            Json(CreateCollectionRequest {
                name: "articles".into(),
                ignore_patterns: Vec::new(),
            }),
        )
        .await
        .unwrap();

        let Json(response) = update_collection_watcher(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionWatcherUpdateRequest {
                enabled: true,
                auto_index_enabled: Some(false),
            }),
        )
        .await
        .unwrap();

        assert!(response.collection.watch_enabled);
        assert!(!response.collection.auto_index_enabled);
        assert!(response.watcher.watch_enabled);
        assert!(!response.watcher.auto_index_enabled);

        let Json(single) =
            collection_watcher_status(State(Arc::clone(&state)), Path("articles".into()))
                .await
                .unwrap();
        assert!(single.collection.watch_enabled);

        let Json(all) = list_collection_watcher_statuses(State(Arc::clone(&state)))
            .await
            .unwrap();
        assert_eq!(all.watchers.len(), 1);
        assert_eq!(all.watchers[0].collection_name, "articles");
    }

    #[tokio::test]
    async fn collection_watcher_maintenance_syncs_members_and_queues_ingest() {
        let test_dir = TestDir::new("collection-watcher-maintenance");
        let root = test_dir.path().join("articles");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("one.md"), "one").unwrap();
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let _ = create_collection(
            State(Arc::clone(&state)),
            Json(CreateCollectionRequest {
                name: "articles".into(),
                ignore_patterns: Vec::new(),
            }),
        )
        .await
        .unwrap();
        let _ = add_collection_root(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(AddCollectionRootRequest {
                path: root.display().to_string(),
            }),
        )
        .await
        .unwrap();
        let _ = update_collection_watcher(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionWatcherUpdateRequest {
                enabled: true,
                auto_index_enabled: Some(true),
            }),
        )
        .await
        .unwrap();
        let running_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, false),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(&state, &running_id)
            .await
            .unwrap();

        let outcome = maintain_collection_after_watch_event(&state, "articles")
            .await
            .unwrap();

        assert_eq!(outcome.added, 1);
        assert_eq!(outcome.queued_task_ids.len(), 1);
        let pipeline_guard = state.pipeline.lock().unwrap();
        let pipeline = pipeline_guard.as_ref().unwrap();
        assert_eq!(
            pipeline
                .store()
                .list_collection_members("articles")
                .unwrap()
                .len(),
            1
        );
        drop(pipeline_guard);
        let store = state.task_store.lock().unwrap();
        let queued = store.queued_tasks(TaskKind::Ingest).unwrap();
        assert!(queued.iter().any(|task| {
            task.request
                .get("source_id")
                .and_then(serde_json::Value::as_str)
                .is_some()
        }));
    }

    #[tokio::test]
    async fn collection_watcher_maintenance_queues_unchanged_pending_bm25_member() {
        let test_dir = TestDir::new("collection-watcher-bm25-pending");
        let root = test_dir.path().join("articles");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("one.md"), "one").unwrap();
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.embedding.enabled = false;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let _ = create_collection(
            State(Arc::clone(&state)),
            Json(CreateCollectionRequest {
                name: "articles".into(),
                ignore_patterns: Vec::new(),
            }),
        )
        .await
        .unwrap();
        let _ = add_collection_root(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(AddCollectionRootRequest {
                path: root.display().to_string(),
            }),
        )
        .await
        .unwrap();
        let Json(sync) = sync_collection(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionSyncRequest {
                paths: Vec::new(),
                max_depth: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(sync.report.added, 1);
        let member_source_id = {
            let pipeline = state.pipeline.lock().unwrap();
            let pipeline = pipeline.as_ref().unwrap();
            let members = pipeline
                .store()
                .list_collection_members("articles")
                .unwrap();
            assert_eq!(members.len(), 1);
            let source = pipeline
                .store()
                .get_source(&members[0].source_id)
                .unwrap()
                .unwrap();
            assert_eq!(source.status, SourceStatus::Pending);
            members[0].source_id.clone()
        };
        let _ = update_collection_watcher(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionWatcherUpdateRequest {
                enabled: true,
                auto_index_enabled: Some(true),
            }),
        )
        .await
        .unwrap();
        let running_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, false),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(&state, &running_id)
            .await
            .unwrap();

        let outcome = maintain_collection_after_watch_event(&state, "articles")
            .await
            .unwrap();

        assert_eq!(outcome.added, 0);
        assert_eq!(outcome.unchanged, 1);
        assert_eq!(outcome.queued_task_ids.len(), 1);
        let store = state.task_store.lock().unwrap();
        let queued = store.queued_tasks(TaskKind::Ingest).unwrap();
        assert!(queued.iter().any(|task| {
            task.request
                .get("source_id")
                .and_then(serde_json::Value::as_str)
                == Some(member_source_id.0.as_str())
        }));
    }

    #[tokio::test]
    async fn collection_watcher_maintenance_skips_added_member_when_source_is_fresh() {
        let test_dir = TestDir::new("collection-watcher-fresh-added-member");
        let root = test_dir.path().join("articles");
        let source_path = root.join("one.md");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source_path, "one fresh source").unwrap();
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.embedding.enabled = false;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let _ = create_collection(
            State(Arc::clone(&state)),
            Json(CreateCollectionRequest {
                name: "articles".into(),
                ignore_patterns: Vec::new(),
            }),
        )
        .await
        .unwrap();
        let _ = add_collection_root(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(AddCollectionRootRequest {
                path: root.display().to_string(),
            }),
        )
        .await
        .unwrap();
        let source_id = run_with_pipeline(Arc::clone(&state), |pipeline| {
            pipeline.add_source(&source_path)
        })
        .unwrap();
        ingest_source_for_test(&state, &source_id).await;
        let _ = update_collection_watcher(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionWatcherUpdateRequest {
                enabled: true,
                auto_index_enabled: Some(true),
            }),
        )
        .await
        .unwrap();

        let outcome = maintain_collection_after_watch_event(&state, "articles")
            .await
            .unwrap();

        assert_eq!(outcome.added, 1);
        assert_eq!(outcome.queued_task_ids, Vec::<String>::new());
        let store = state.task_store.lock().unwrap();
        let active_duplicate = store
            .active_tasks(TaskKind::Ingest)
            .unwrap()
            .into_iter()
            .filter(|task| {
                task.request
                    .get("source_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(source_id.0.as_str())
            })
            .count();
        assert_eq!(active_duplicate, 0);
    }

    #[tokio::test]
    async fn collection_watcher_maintenance_queues_only_new_and_modified_sources() {
        let test_dir = TestDir::new("collection-watcher-new-modified");
        let root = test_dir.path().join("articles");
        let unchanged_path = root.join("one.md");
        let modified_path = root.join("two.md");
        let added_path = root.join("three.md");
        fs::create_dir_all(&root).unwrap();
        fs::write(&unchanged_path, "one unchanged source").unwrap();
        fs::write(&modified_path, "two original source").unwrap();
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.embedding.enabled = false;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let _ = create_collection(
            State(Arc::clone(&state)),
            Json(CreateCollectionRequest {
                name: "articles".into(),
                ignore_patterns: Vec::new(),
            }),
        )
        .await
        .unwrap();
        let _ = add_collection_root(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(AddCollectionRootRequest {
                path: root.display().to_string(),
            }),
        )
        .await
        .unwrap();
        let Json(sync) = sync_collection(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionSyncRequest {
                paths: Vec::new(),
                max_depth: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(sync.report.added, 2);
        let (unchanged_id, modified_id) = collection_member_ids_for_test(&state, "articles");
        ingest_source_for_test(&state, &unchanged_id).await;
        ingest_source_for_test(&state, &modified_id).await;
        fs::write(&modified_path, "two modified source").unwrap();
        fs::write(&added_path, "three new source").unwrap();
        let _ = update_collection_watcher(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionWatcherUpdateRequest {
                enabled: true,
                auto_index_enabled: Some(true),
            }),
        )
        .await
        .unwrap();
        let running_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, false),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(&state, &running_id)
            .await
            .unwrap();

        let outcome = maintain_collection_after_watch_event(&state, "articles")
            .await
            .unwrap();

        assert_eq!(outcome.queued_task_ids.len(), 2);
        let added_id = SourceId::from_path(&fs::canonicalize(&added_path).unwrap());
        let queued_source_ids = queued_source_ids_for_test(&state);
        assert!(queued_source_ids.contains(&modified_id));
        assert!(queued_source_ids.contains(&added_id));
        assert!(!queued_source_ids.contains(&unchanged_id));
    }

    #[tokio::test]
    async fn collection_watcher_maintenance_repeated_same_fingerprint_queues_once() {
        let test_dir = TestDir::new("collection-watcher-repeated-source-fingerprint");
        let root = test_dir.path().join("articles");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("one.md"), "one pending source").unwrap();
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let _ = create_collection(
            State(Arc::clone(&state)),
            Json(CreateCollectionRequest {
                name: "articles".into(),
                ignore_patterns: Vec::new(),
            }),
        )
        .await
        .unwrap();
        let _ = add_collection_root(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(AddCollectionRootRequest {
                path: root.display().to_string(),
            }),
        )
        .await
        .unwrap();
        let Json(sync) = sync_collection(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionSyncRequest {
                paths: Vec::new(),
                max_depth: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(sync.report.added, 1);
        let source_id = collection_member_ids_for_test(&state, "articles").0;
        let source_hash = current_source_hash_for_test(&state, &source_id);
        let _ = update_collection_watcher(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionWatcherUpdateRequest {
                enabled: true,
                auto_index_enabled: Some(true),
            }),
        )
        .await
        .unwrap();
        let running_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, false),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(&state, &running_id)
            .await
            .unwrap();

        let first = maintain_collection_after_watch_event(&state, "articles")
            .await
            .unwrap();
        let second = maintain_collection_after_watch_event(&state, "articles")
            .await
            .unwrap();

        assert_eq!(first.queued_task_ids.len(), 1);
        assert_eq!(second.queued_task_ids, Vec::<String>::new());
        assert_eq!(
            active_ingest_intent_count_for_test(&state, &source_id, &source_hash),
            1
        );
    }

    #[tokio::test]
    async fn collection_watcher_atomic_enqueue_dedupes_concurrent_same_fingerprint() {
        let test_dir = TestDir::new("collection-watcher-concurrent-source-fingerprint");
        let root = test_dir.path().join("articles");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("one.md"), "one pending source").unwrap();
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let _ = create_collection(
            State(Arc::clone(&state)),
            Json(CreateCollectionRequest {
                name: "articles".into(),
                ignore_patterns: Vec::new(),
            }),
        )
        .await
        .unwrap();
        let _ = add_collection_root(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(AddCollectionRootRequest {
                path: root.display().to_string(),
            }),
        )
        .await
        .unwrap();
        let Json(sync) = sync_collection(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionSyncRequest {
                paths: Vec::new(),
                max_depth: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(sync.report.added, 1);
        let source_id = collection_member_ids_for_test(&state, "articles").0;
        let source_hash = current_source_hash_for_test(&state, &source_id);
        let worker_count = 8;
        let barrier = Arc::new(tokio::sync::Barrier::new(worker_count));
        let mut handles = Vec::new();
        for _ in 0..worker_count {
            let state = Arc::clone(&state);
            let barrier = Arc::clone(&barrier);
            let source_id = source_id.clone();
            let source_hash = source_hash.clone();
            handles.push(tokio::spawn(async move {
                barrier.wait().await;
                create_collection_watcher_ingest_task_if_no_active_intent(
                    &state,
                    "articles",
                    CollectionMaintenanceIngestCandidate {
                        source_id,
                        reason: "stale",
                        source_hash: Some(source_hash),
                    },
                )
                .await
            }));
        }

        let mut created = Vec::new();
        for handle in handles {
            if let Some(task_id) = handle.await.unwrap().unwrap() {
                created.push(task_id);
            }
        }

        assert_eq!(created.len(), 1);
        assert_eq!(
            active_ingest_intent_count_for_test(&state, &source_id, &source_hash),
            1
        );
    }

    #[tokio::test]
    async fn collection_watcher_maintenance_skips_running_source_ingest_intent() {
        let test_dir = TestDir::new("collection-watcher-running-source-intent");
        let root = test_dir.path().join("articles");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("one.md"), "one pending source").unwrap();
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let _ = create_collection(
            State(Arc::clone(&state)),
            Json(CreateCollectionRequest {
                name: "articles".into(),
                ignore_patterns: Vec::new(),
            }),
        )
        .await
        .unwrap();
        let _ = add_collection_root(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(AddCollectionRootRequest {
                path: root.display().to_string(),
            }),
        )
        .await
        .unwrap();
        let Json(sync) = sync_collection(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionSyncRequest {
                paths: Vec::new(),
                max_depth: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(sync.report.added, 1);
        let source_id = collection_member_ids_for_test(&state, "articles").0;
        let source_hash = current_source_hash_for_test(&state, &source_id);
        let running_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_request_with_source_hash_for_test(&source_id, &source_hash),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(&state, &running_id)
            .await
            .unwrap();
        let _ = update_collection_watcher(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionWatcherUpdateRequest {
                enabled: true,
                auto_index_enabled: Some(true),
            }),
        )
        .await
        .unwrap();

        let outcome = maintain_collection_after_watch_event(&state, "articles")
            .await
            .unwrap();

        assert_eq!(outcome.queued_task_ids, Vec::<String>::new());
        let store = state.task_store.lock().unwrap();
        let active_for_source = store
            .active_tasks(TaskKind::Ingest)
            .unwrap()
            .into_iter()
            .filter(|task| {
                task.request
                    .get("source_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(source_id.0.as_str())
            })
            .count();
        assert_eq!(active_for_source, 1);
    }

    #[tokio::test]
    async fn collection_watcher_maintenance_skips_production_ingest_intent_with_source_hash() {
        let test_dir = TestDir::new("collection-watcher-production-intent-source-hash");
        let root = test_dir.path().join("articles");
        fs::create_dir_all(&root).unwrap();
        fs::write(root.join("one.md"), "one production source").unwrap();
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let _ = create_collection(
            State(Arc::clone(&state)),
            Json(CreateCollectionRequest {
                name: "articles".into(),
                ignore_patterns: Vec::new(),
            }),
        )
        .await
        .unwrap();
        let _ = add_collection_root(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(AddCollectionRootRequest {
                path: root.display().to_string(),
            }),
        )
        .await
        .unwrap();
        let Json(sync) = sync_collection(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionSyncRequest {
                paths: Vec::new(),
                max_depth: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(sync.report.added, 1);
        let source_id = collection_member_ids_for_test(&state, "articles").0;
        let source_hash = current_source_hash_for_test(&state, &source_id);
        state.ingest_queue_active.store(true, Ordering::Release);
        let Json(created) = submit_ingest_task(
            State(Arc::clone(&state)),
            Json(TaskIngestRequest {
                source_id: Some(source_id.0.clone()),
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap();
        state.ingest_queue_active.store(false, Ordering::Release);
        let running = claim_startable_ingest_task(&state).await.unwrap().unwrap();
        assert_eq!(running.id.0, created.task_id);
        assert_eq!(
            running
                .request
                .get("source_hash")
                .and_then(serde_json::Value::as_str),
            Some(source_hash.as_str())
        );
        let _ = update_collection_watcher(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionWatcherUpdateRequest {
                enabled: true,
                auto_index_enabled: Some(true),
            }),
        )
        .await
        .unwrap();

        let outcome = maintain_collection_after_watch_event(&state, "articles")
            .await
            .unwrap();

        assert_eq!(outcome.queued_task_ids, Vec::<String>::new());
        let store = state.task_store.lock().unwrap();
        let active_for_source = store
            .active_tasks(TaskKind::Ingest)
            .unwrap()
            .into_iter()
            .filter(|task| {
                task.request
                    .get("source_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(source_id.0.as_str())
            })
            .count();
        assert_eq!(active_for_source, 1);
    }

    #[tokio::test]
    async fn collection_watcher_maintenance_queues_changed_source_when_active_task_is_hashless() {
        let test_dir = TestDir::new("collection-watcher-running-source-hashless");
        let root = test_dir.path().join("articles");
        let source_path = root.join("one.md");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source_path, "one original source").unwrap();
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.embedding.enabled = false;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let _ = create_collection(
            State(Arc::clone(&state)),
            Json(CreateCollectionRequest {
                name: "articles".into(),
                ignore_patterns: Vec::new(),
            }),
        )
        .await
        .unwrap();
        let _ = add_collection_root(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(AddCollectionRootRequest {
                path: root.display().to_string(),
            }),
        )
        .await
        .unwrap();
        let Json(sync) = sync_collection(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionSyncRequest {
                paths: Vec::new(),
                max_depth: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(sync.report.added, 1);
        let source_id = collection_member_ids_for_test(&state, "articles").0;
        let running_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(
                Some(&source_id.0),
                false,
                None,
                false,
                true,
            ),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(&state, &running_id)
            .await
            .unwrap();
        fs::write(&source_path, "one modified source").unwrap();
        let current_hash = current_source_hash_for_test(&state, &source_id);
        let _ = update_collection_watcher(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionWatcherUpdateRequest {
                enabled: true,
                auto_index_enabled: Some(true),
            }),
        )
        .await
        .unwrap();

        let outcome = maintain_collection_after_watch_event(&state, "articles")
            .await
            .unwrap();

        assert_eq!(outcome.queued_task_ids.len(), 1);
        let store = state.task_store.lock().unwrap();
        let queued = store.queued_tasks(TaskKind::Ingest).unwrap();
        let queued_for_source = queued
            .iter()
            .filter(|task| {
                task.request
                    .get("source_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(source_id.0.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(queued_for_source.len(), 1);
        assert_eq!(
            queued_for_source[0]
                .request
                .get("source_hash")
                .and_then(serde_json::Value::as_str),
            Some(current_hash.as_str())
        );
    }

    #[tokio::test]
    async fn collection_watcher_maintenance_queues_changed_source_when_active_task_is_vectors_only()
    {
        let test_dir = TestDir::new("collection-watcher-running-source-vectors-only");
        let root = test_dir.path().join("articles");
        let source_path = root.join("one.md");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source_path, "one original source").unwrap();
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.embedding.enabled = false;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let _ = create_collection(
            State(Arc::clone(&state)),
            Json(CreateCollectionRequest {
                name: "articles".into(),
                ignore_patterns: Vec::new(),
            }),
        )
        .await
        .unwrap();
        let _ = add_collection_root(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(AddCollectionRootRequest {
                path: root.display().to_string(),
            }),
        )
        .await
        .unwrap();
        let Json(sync) = sync_collection(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionSyncRequest {
                paths: Vec::new(),
                max_depth: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(sync.report.added, 1);
        let source_id = collection_member_ids_for_test(&state, "articles").0;
        fs::write(&source_path, "one modified source").unwrap();
        let current_hash = current_source_hash_for_test(&state, &source_id);
        let running_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_request_with_source_hash_and_vectors_only_for_test(
                &source_id,
                &current_hash,
                true,
            ),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(&state, &running_id)
            .await
            .unwrap();
        let _ = update_collection_watcher(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionWatcherUpdateRequest {
                enabled: true,
                auto_index_enabled: Some(true),
            }),
        )
        .await
        .unwrap();

        let outcome = maintain_collection_after_watch_event(&state, "articles")
            .await
            .unwrap();

        assert_eq!(outcome.queued_task_ids.len(), 1);
        let store = state.task_store.lock().unwrap();
        let queued = store.queued_tasks(TaskKind::Ingest).unwrap();
        let queued_for_source = queued
            .iter()
            .filter(|task| {
                task.request
                    .get("source_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(source_id.0.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(queued_for_source.len(), 1);
        assert_eq!(
            queued_for_source[0]
                .request
                .get("source_hash")
                .and_then(serde_json::Value::as_str),
            Some(current_hash.as_str())
        );
    }

    #[tokio::test]
    async fn collection_watcher_maintenance_queues_changed_source_when_running_hash_is_old() {
        let test_dir = TestDir::new("collection-watcher-running-source-old-hash");
        let root = test_dir.path().join("articles");
        let source_path = root.join("one.md");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source_path, "one original source").unwrap();
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.embedding.enabled = false;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let _ = create_collection(
            State(Arc::clone(&state)),
            Json(CreateCollectionRequest {
                name: "articles".into(),
                ignore_patterns: Vec::new(),
            }),
        )
        .await
        .unwrap();
        let _ = add_collection_root(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(AddCollectionRootRequest {
                path: root.display().to_string(),
            }),
        )
        .await
        .unwrap();
        let Json(sync) = sync_collection(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionSyncRequest {
                paths: Vec::new(),
                max_depth: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(sync.report.added, 1);
        let source_id = collection_member_ids_for_test(&state, "articles").0;
        let old_hash = current_source_hash_for_test(&state, &source_id);
        let running_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_request_with_source_hash_for_test(&source_id, &old_hash),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(&state, &running_id)
            .await
            .unwrap();
        fs::write(&source_path, "one modified source").unwrap();
        let current_hash = current_source_hash_for_test(&state, &source_id);
        assert_ne!(current_hash, old_hash);
        let _ = update_collection_watcher(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionWatcherUpdateRequest {
                enabled: true,
                auto_index_enabled: Some(true),
            }),
        )
        .await
        .unwrap();

        let outcome = maintain_collection_after_watch_event(&state, "articles")
            .await
            .unwrap();

        assert_eq!(outcome.queued_task_ids.len(), 1);
        let store = state.task_store.lock().unwrap();
        let queued = store.queued_tasks(TaskKind::Ingest).unwrap();
        let queued_for_source = queued
            .iter()
            .filter(|task| {
                task.request
                    .get("source_id")
                    .and_then(serde_json::Value::as_str)
                    == Some(source_id.0.as_str())
            })
            .collect::<Vec<_>>();
        assert_eq!(queued_for_source.len(), 1);
        assert_eq!(
            queued_for_source[0]
                .request
                .get("source_hash")
                .and_then(serde_json::Value::as_str),
            Some(current_hash.as_str())
        );
    }

    #[tokio::test]
    async fn collection_watcher_maintenance_deleted_file_does_not_queue_ingest_or_repeat_generation(
    ) {
        let test_dir = TestDir::new("collection-watcher-deleted-file-no-ingest");
        let root = test_dir.path().join("articles");
        let source_path = root.join("one.md");
        fs::create_dir_all(&root).unwrap();
        fs::write(&source_path, "one indexed source").unwrap();
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.embedding.enabled = false;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let _ = create_collection(
            State(Arc::clone(&state)),
            Json(CreateCollectionRequest {
                name: "articles".into(),
                ignore_patterns: Vec::new(),
            }),
        )
        .await
        .unwrap();
        let _ = add_collection_root(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(AddCollectionRootRequest {
                path: root.display().to_string(),
            }),
        )
        .await
        .unwrap();
        let Json(sync) = sync_collection(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionSyncRequest {
                paths: Vec::new(),
                max_depth: None,
            }),
        )
        .await
        .unwrap();
        assert_eq!(sync.report.added, 1);
        let source_id = collection_member_ids_for_test(&state, "articles").0;
        ingest_source_for_test(&state, &source_id).await;
        let _ = update_collection_watcher(
            State(Arc::clone(&state)),
            Path("articles".into()),
            Json(CollectionWatcherUpdateRequest {
                enabled: true,
                auto_index_enabled: Some(true),
            }),
        )
        .await
        .unwrap();
        fs::remove_file(&source_path).unwrap();

        let first = maintain_collection_after_watch_event(&state, "articles")
            .await
            .unwrap();

        assert_eq!(first.queued_task_ids, Vec::<String>::new());
        assert!(queued_source_ids_for_test(&state).is_empty());
        let generation_after_delete = {
            let pipeline = state.pipeline.lock().unwrap();
            let pipeline = pipeline.as_ref().unwrap();
            assert_eq!(
                pipeline
                    .source_ingest_snapshot(&source_id)
                    .unwrap()
                    .freshness,
                SourceIngestFreshness::Missing
            );
            pipeline.store().index_generation().unwrap()
        };

        let second = maintain_collection_after_watch_event(&state, "articles")
            .await
            .unwrap();

        assert_eq!(second.queued_task_ids, Vec::<String>::new());
        assert!(queued_source_ids_for_test(&state).is_empty());
        let generation_after_repeat = {
            let pipeline = state.pipeline.lock().unwrap();
            let pipeline = pipeline.as_ref().unwrap();
            pipeline.store().index_generation().unwrap()
        };
        assert_eq!(generation_after_repeat, generation_after_delete);
    }

    #[test]
    fn collection_watcher_debounce_coalesces_collection_names() {
        let mut debounced = DebouncedCollectionSet::default();

        assert!(debounced.insert_many(vec!["b".to_string(), "a".to_string()]));
        assert!(!debounced.insert_many(vec!["a".to_string()]));
        assert_eq!(debounced.drain(), vec!["a".to_string(), "b".to_string()]);
        assert!(debounced.is_empty());
    }

    #[test]
    fn collection_watch_path_changes_rewatch_mode_changes() {
        let path = PathBuf::from("/tmp/verbatim/articles");
        let watched_paths = BTreeMap::from([(path.clone(), RecursiveMode::NonRecursive)]);
        let mut plan = CollectionWatchPlan::default();
        plan.roots.insert(
            path.clone(),
            CollectionWatchRoot {
                recursive: RecursiveMode::Recursive,
                collections: BTreeSet::from(["articles".to_string()]),
            },
        );

        let changes = collection_watch_path_changes(&watched_paths, &plan);

        assert_eq!(
            changes,
            vec![
                CollectionWatchPathChange::Unwatch(path.clone()),
                CollectionWatchPathChange::Watch {
                    path,
                    recursive: RecursiveMode::Recursive,
                },
            ]
        );
    }

    #[test]
    fn collection_watcher_ignores_configured_paths() {
        let patterns = vec!["ignored/".to_string(), "*.tmp".to_string()];

        assert!(collection_watcher_path_ignored(
            FsPath::new("/tmp/root/ignored/file.md"),
            &patterns
        ));
        assert!(collection_watcher_path_ignored(
            FsPath::new("/tmp/root/scratch.tmp"),
            &patterns
        ));
        assert!(!collection_watcher_path_ignored(
            FsPath::new("/tmp/root/keep.md"),
            &patterns
        ));
    }

    #[tokio::test]
    async fn retrieve_handler_returns_context_pack_when_chat_is_disabled_and_unavailable() {
        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("retrieve-chat-disabled");
        let source_path = test_dir.path().join("doc.md");
        fs::write(
            &source_path,
            "Alpha retrieval evidence answers the context-only question.",
        )
        .unwrap();
        let config = retrieve_test_config(&model_server.base_url);
        assert!(!config.chat.enabled);

        let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        pipeline.ingest_source(&source_id).await.unwrap();

        let state = test_state(config, test_dir.path(), pipeline);
        let response = retrieve(
            State(state),
            Json(RetrieveRequest {
                question: "Alpha context-only question?".into(),
                source_id: Some(source_id.0.clone()),
                collection_filter: CollectionFilterRequest::default(),
                embedding_profile_id: None,
                limit: Some(3),
                page_size: Some(1),
                page: Some(1),
                fast: true,
                rerank: Some(false),
                dense_top_k: None,
                bm25_top_k: None,
                rerank_top_n: None,
                bypass_cache: false,
                include_debug: false,
                include_debug_packs: false,
                include_locator: false,
                passage: false,
            }),
        )
        .await
        .unwrap()
        .0;

        assert_eq!(response.source_id.as_deref(), Some(source_id.0.as_str()));
        assert_eq!(response.returned_results, 1);
        assert_eq!(response.results[0].label, "E1");
        assert!(response.results[0]
            .snippet
            .contains("Alpha retrieval evidence"));
        assert!(model_server.embedding_requests() >= 2);
        assert_eq!(model_server.chat_requests(), 0);
    }

    #[tokio::test]
    async fn capability_refresh_direct_retrieve_rejects_reset_profile_vectors() {
        let (_test_dir, state, source_id, model_server) =
            reloaded_embedding_endpoint_state("capability-refresh-retrieve", false).await;

        let (status, Json(body)) = retrieve(
            State(Arc::clone(&state)),
            Json(RetrieveRequest {
                question: "Alpha drifted endpoint question?".into(),
                source_id: Some(source_id.0.clone()),
                collection_filter: CollectionFilterRequest::default(),
                embedding_profile_id: None,
                limit: Some(3),
                page_size: Some(1),
                page: Some(1),
                fast: true,
                rerank: Some(false),
                dense_top_k: None,
                bm25_top_k: None,
                rerank_top_n: None,
                bypass_cache: false,
                include_debug: false,
                include_debug_packs: false,
                include_locator: false,
                passage: false,
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body
            .error
            .contains("embedding profile 'default' has no vectors"));
        assert_eq!(model_server.embedding_requests(), 0);
        assert_eq!(default_profile_source_vector_count(&state, &source_id), 0);
    }

    #[tokio::test]
    async fn capability_refresh_ask_rejects_reset_profile_vectors_before_chat() {
        let (_test_dir, state, source_id, model_server) =
            reloaded_embedding_endpoint_state("capability-refresh-ask", true).await;

        let (status, Json(body)) = ask(
            State(Arc::clone(&state)),
            Json(AskRequest {
                question: "Alpha drifted endpoint ask?".into(),
                source_id: Some(source_id.0.clone()),
                collection_filter: CollectionFilterRequest::default(),
                embedding_profile_id: None,
                show_retrieval: false,
                context_only: false,
                limit: None,
                page_size: None,
                page: None,
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body
            .error
            .contains("embedding profile 'default' has no vectors"));
        assert_eq!(model_server.embedding_requests(), 0);
        assert_eq!(model_server.chat_requests(), 0);
        assert_eq!(default_profile_source_vector_count(&state, &source_id), 0);
    }

    #[tokio::test]
    async fn capability_refresh_background_batch_expansion_marks_reset_sources_stale() {
        let (_test_dir, state, source_id, _model_server) =
            reloaded_embedding_endpoint_state("capability-refresh-background-batch", false).await;

        let expanded = background_ingest_batch_sources(&state, false, false)
            .await
            .unwrap();
        let expanded_source_ids = expanded
            .sources
            .iter()
            .map(|candidate| candidate.source_id.clone())
            .collect::<Vec<_>>();

        assert_eq!(expanded_source_ids, vec![source_id.clone()]);
        let source = state
            .pipeline
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .store()
            .get_source(&source_id)
            .unwrap()
            .unwrap();
        assert_eq!(source.status, SourceStatus::Stale);
        assert_eq!(default_profile_source_vector_count(&state, &source_id), 0);
    }

    #[tokio::test]
    async fn collection_require_fresh_retrieve_refreshes_profile_before_vector_check() {
        let (_test_dir, state, source_id, model_server) =
            reloaded_embedding_endpoint_state("capability-refresh-collection-retrieve", false)
                .await;
        add_single_source_collection_for_test(&state, "articles", &source_id);

        let (status, Json(body)) = retrieve(
            State(Arc::clone(&state)),
            Json(RetrieveRequest {
                question: "Alpha drifted endpoint collection question?".into(),
                source_id: None,
                collection_filter: CollectionFilterRequest {
                    collection_ids: Vec::new(),
                    names: vec!["articles".into()],
                    require_fresh: true,
                },
                embedding_profile_id: None,
                limit: Some(3),
                page_size: Some(1),
                page: Some(1),
                fast: true,
                rerank: Some(false),
                dense_top_k: None,
                bm25_top_k: None,
                rerank_top_n: None,
                bypass_cache: false,
                include_debug: false,
                include_debug_packs: false,
                include_locator: false,
                passage: false,
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.error.contains("collection filter requires fresh"));
        assert!(body
            .error
            .contains(&format!("verbatim ingest {}", source_id.0)));
        assert!(body.error.contains("verbatim reindex --stale"));
        assert!(!body.error.contains("has no vectors"));
        assert_eq!(model_server.embedding_requests(), 0);
        assert_eq!(default_profile_source_vector_count(&state, &source_id), 0);
    }

    #[tokio::test]
    async fn collection_require_fresh_ask_refreshes_profile_before_vector_check() {
        let (_test_dir, state, source_id, model_server) =
            reloaded_embedding_endpoint_state("capability-refresh-collection-ask", true).await;
        add_single_source_collection_for_test(&state, "articles", &source_id);

        let (status, Json(body)) = ask(
            State(Arc::clone(&state)),
            Json(AskRequest {
                question: "Alpha drifted endpoint collection ask?".into(),
                source_id: None,
                collection_filter: CollectionFilterRequest {
                    collection_ids: Vec::new(),
                    names: vec!["articles".into()],
                    require_fresh: true,
                },
                embedding_profile_id: None,
                show_retrieval: false,
                context_only: false,
                limit: None,
                page_size: None,
                page: None,
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert!(body.error.contains("collection filter requires fresh"));
        assert!(body
            .error
            .contains(&format!("verbatim ingest {}", source_id.0)));
        assert!(!body.error.contains("has no vectors"));
        assert_eq!(model_server.embedding_requests(), 0);
        assert_eq!(model_server.chat_requests(), 0);
        assert_eq!(default_profile_source_vector_count(&state, &source_id), 0);
    }

    #[tokio::test]
    async fn retrieve_handler_low_memory_dense_search_uses_stored_vectors_without_resident_hnsw() {
        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("retrieve-low-memory-vectors");
        let source_path = test_dir.path().join("doc.md");
        fs::write(
            &source_path,
            "Alpha low-memory dense retrieval should use stored vectors.",
        )
        .unwrap();
        let mut config = retrieve_test_config(&model_server.base_url);
        config.vector_index.residency = VectorIndexResidency::LowMemory;

        let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        pipeline.ingest_source(&source_id).await.unwrap();
        assert!(pipeline.hnsw().is_empty());
        assert!(
            pipeline
                .store()
                .count_vector_documents_for_profile(
                    &EmbeddingProfileId::default_profile(),
                    Some(&source_id),
                )
                .unwrap()
                > 0
        );

        let state = test_state(config, test_dir.path(), pipeline);
        let response = retrieve(
            State(state),
            Json(RetrieveRequest {
                question: "Alpha dense retrieval?".into(),
                source_id: Some(source_id.0.clone()),
                collection_filter: CollectionFilterRequest::default(),
                embedding_profile_id: None,
                limit: Some(3),
                page_size: Some(1),
                page: Some(1),
                fast: true,
                rerank: Some(false),
                dense_top_k: None,
                bm25_top_k: None,
                rerank_top_n: None,
                bypass_cache: false,
                include_debug: true,
                include_debug_packs: false,
                include_locator: false,
                passage: false,
            }),
        )
        .await
        .unwrap()
        .0;

        let debug = response.debug.expect("retrieval debug");
        assert_eq!(
            debug.dense_vector_path,
            RetrievalDenseVectorPath::LowMemorySqliteScan
        );
        assert!(!debug.dense_hits.is_empty());
        assert_eq!(model_server.chat_requests(), 0);
    }

    #[tokio::test]
    async fn retrieve_handler_resident_hnsw_dense_search_remains_configurable() {
        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("retrieve-resident-hnsw");
        let source_path = test_dir.path().join("doc.md");
        fs::write(
            &source_path,
            "Alpha resident HNSW dense retrieval should use the loaded index.",
        )
        .unwrap();
        let mut config = retrieve_test_config(&model_server.base_url);
        config.vector_index.residency = VectorIndexResidency::ResidentHnsw;

        let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        pipeline.ingest_source(&source_id).await.unwrap();
        assert!(!pipeline.hnsw().is_empty());

        let state = test_state(config, test_dir.path(), pipeline);
        let response = retrieve(
            State(state),
            Json(RetrieveRequest {
                question: "Alpha dense retrieval?".into(),
                source_id: Some(source_id.0.clone()),
                collection_filter: CollectionFilterRequest::default(),
                embedding_profile_id: None,
                limit: Some(3),
                page_size: Some(1),
                page: Some(1),
                fast: true,
                rerank: Some(false),
                dense_top_k: None,
                bm25_top_k: None,
                rerank_top_n: None,
                bypass_cache: false,
                include_debug: true,
                include_debug_packs: false,
                include_locator: false,
                passage: false,
            }),
        )
        .await
        .unwrap()
        .0;

        let debug = response.debug.expect("retrieval debug");
        assert_eq!(
            debug.dense_vector_path,
            RetrievalDenseVectorPath::ResidentHnsw
        );
        assert!(!debug.dense_hits.is_empty());
        assert_eq!(model_server.chat_requests(), 0);
    }

    #[tokio::test]
    async fn retrieve_handler_filters_by_materialized_collections_with_provenance() {
        use verbatim_core::collection::{CollectionMemberCandidate, CollectionSyncReport};

        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("retrieve-collection-filter");
        let root = test_dir.path().join("articles");
        fs::create_dir_all(&root).unwrap();
        let ares_path = root.join("Areskapitalon-notes.md");
        let other_path = root.join("general.md");
        let outside_path = test_dir.path().join("outside.md");
        fs::write(&ares_path, "Areskapitalon alpha evidence inside articles.").unwrap();
        fs::write(&other_path, "General beta article evidence.").unwrap();
        fs::write(
            &outside_path,
            "Areskapitalon alpha evidence outside collections.",
        )
        .unwrap();
        let config = retrieve_test_config(&model_server.base_url);
        let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let ares_id = pipeline.add_source(&ares_path).unwrap();
        let other_id = pipeline.add_source(&other_path).unwrap();
        let outside_id = pipeline.add_source(&outside_path).unwrap();
        pipeline.ingest_source(&ares_id).await.unwrap();
        pipeline.ingest_source(&other_id).await.unwrap();
        pipeline.ingest_source(&outside_id).await.unwrap();
        let report = || CollectionSyncReport {
            member_count: 0,
            added: 0,
            removed: 0,
            unchanged: 0,
            scanned_roots: 1,
            max_depth: 32,
            skipped: Vec::new(),
        };
        let ares_candidate = CollectionMemberCandidate {
            source_id: ares_id.clone(),
            logical_path: "Areskapitalon-notes.md".into(),
            source_path: fs::canonicalize(&ares_path).unwrap(),
        };
        let other_candidate = CollectionMemberCandidate {
            source_id: other_id.clone(),
            logical_path: "general.md".into(),
            source_path: fs::canonicalize(&other_path).unwrap(),
        };
        pipeline.store().create_collection("articles", &[]).unwrap();
        pipeline
            .store()
            .create_collection("areskapitalon", &[])
            .unwrap();
        pipeline
            .store()
            .replace_collection_members(
                "articles",
                &[ares_candidate.clone(), other_candidate],
                report(),
            )
            .unwrap();
        pipeline
            .store()
            .replace_collection_members("areskapitalon", &[ares_candidate], report())
            .unwrap();
        pipeline
            .store()
            .update_source_status(&other_id, &SourceStatus::Stale)
            .unwrap();
        let ares_id_text = ares_id.0.clone();
        let outside_id_text = outside_id.0.clone();
        let state = test_state(config, test_dir.path(), pipeline);

        let response = retrieve(
            State(state),
            Json(RetrieveRequest {
                question: "Areskapitalon alpha evidence?".into(),
                source_id: None,
                collection_filter: CollectionFilterRequest {
                    collection_ids: Vec::new(),
                    names: vec!["articles".into(), "areskapitalon".into()],
                    require_fresh: false,
                },
                embedding_profile_id: None,
                limit: Some(3),
                page_size: Some(3),
                page: Some(1),
                fast: true,
                rerank: Some(false),
                dense_top_k: None,
                bm25_top_k: None,
                rerank_top_n: None,
                bypass_cache: false,
                include_debug: false,
                include_debug_packs: false,
                include_locator: true,
                passage: false,
            }),
        )
        .await
        .unwrap()
        .0;

        let collection_filter = response.collection_filter.as_ref().unwrap();
        assert_eq!(collection_filter.applied.len(), 2);
        assert_eq!(collection_filter.union_source_count, 2);
        assert!(collection_filter.stale);
        assert!(collection_filter
            .warnings
            .iter()
            .any(|warning| warning.contains("not currently indexed")));
        assert!(response
            .results
            .iter()
            .all(|result| result.source_id != outside_id_text));
        let ares_result = response
            .results
            .iter()
            .find(|result| result.source_id == ares_id_text)
            .expect("Areskapitalon result");
        assert_eq!(ares_result.collections.len(), 2);
        assert!(ares_result
            .collections
            .iter()
            .any(|collection| collection.name == "articles"));
        assert!(ares_result
            .collections
            .iter()
            .any(|collection| collection.name == "areskapitalon"));
    }

    #[tokio::test]
    async fn collection_require_fresh_error_prints_copyable_reingest_commands() {
        use verbatim_core::collection::{CollectionMemberCandidate, CollectionSyncReport};

        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("collection-fresh-error-remediation");
        let source_path = test_dir.path().join("Areskapitalon-stale.md");
        fs::write(&source_path, "Areskapitalon stale evidence needs indexing.").unwrap();
        let config = retrieve_test_config(&model_server.base_url);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        pipeline
            .store()
            .create_collection("areskapitalon", &[])
            .unwrap();
        pipeline
            .store()
            .replace_collection_members(
                "areskapitalon",
                &[CollectionMemberCandidate {
                    source_id: source_id.clone(),
                    logical_path: "Areskapitalon-stale.md".into(),
                    source_path: fs::canonicalize(&source_path).unwrap(),
                }],
                CollectionSyncReport {
                    member_count: 0,
                    added: 0,
                    removed: 0,
                    unchanged: 0,
                    scanned_roots: 1,
                    max_depth: 32,
                    skipped: Vec::new(),
                },
            )
            .unwrap();
        let source_id_text = source_id.0.clone();
        let state = test_state(config, test_dir.path(), pipeline);

        let error = retrieve(
            State(state),
            Json(RetrieveRequest {
                question: "Areskapitalon evidence?".into(),
                source_id: None,
                collection_filter: CollectionFilterRequest {
                    collection_ids: Vec::new(),
                    names: vec!["areskapitalon".into()],
                    require_fresh: true,
                },
                embedding_profile_id: None,
                limit: Some(3),
                page_size: Some(1),
                page: Some(1),
                fast: true,
                rerank: Some(false),
                dense_top_k: None,
                bm25_top_k: None,
                rerank_top_n: None,
                bypass_cache: false,
                include_debug: false,
                include_debug_packs: false,
                include_locator: false,
                passage: false,
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(error.0, StatusCode::BAD_REQUEST);
        let message = error.1 .0.error;
        assert!(!message.contains("verbatim collection sync areskapitalon"));
        assert!(message.contains(&format!("verbatim ingest {source_id_text}")));
        assert!(message.contains("verbatim ask --collection areskapitalon --require-fresh"));
    }

    #[tokio::test]
    async fn ask_context_only_handler_uses_collection_filter() {
        use verbatim_core::collection::{CollectionMemberCandidate, CollectionSyncReport};

        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("ask-context-only-collection-filter");
        let inside_path = test_dir.path().join("inside.md");
        let outside_path = test_dir.path().join("outside.md");
        fs::write(&inside_path, "Alpha collection-scoped ask evidence.").unwrap();
        fs::write(
            &outside_path,
            "Alpha outside evidence must not appear in collection ask.",
        )
        .unwrap();
        let config = retrieve_test_config(&model_server.base_url);
        let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let inside_id = pipeline.add_source(&inside_path).unwrap();
        let outside_id = pipeline.add_source(&outside_path).unwrap();
        pipeline.ingest_source(&inside_id).await.unwrap();
        pipeline.ingest_source(&outside_id).await.unwrap();
        pipeline.store().create_collection("articles", &[]).unwrap();
        pipeline
            .store()
            .replace_collection_members(
                "articles",
                &[CollectionMemberCandidate {
                    source_id: inside_id.clone(),
                    logical_path: "inside.md".into(),
                    source_path: fs::canonicalize(&inside_path).unwrap(),
                }],
                CollectionSyncReport {
                    member_count: 0,
                    added: 0,
                    removed: 0,
                    unchanged: 0,
                    scanned_roots: 1,
                    max_depth: 32,
                    skipped: Vec::new(),
                },
            )
            .unwrap();
        let inside_id_text = inside_id.0.clone();
        let outside_id_text = outside_id.0.clone();
        let state = test_state(config, test_dir.path(), pipeline);

        let response = ask(
            State(state),
            Json(AskRequest {
                question: "Alpha ask evidence?".into(),
                source_id: None,
                collection_filter: CollectionFilterRequest {
                    collection_ids: Vec::new(),
                    names: vec!["articles".into()],
                    require_fresh: false,
                },
                embedding_profile_id: None,
                show_retrieval: false,
                context_only: true,
                limit: None,
                page_size: None,
                page: None,
            }),
        )
        .await
        .unwrap()
        .0;

        assert!(response.collection_filter.is_some());
        let context = response.context.expect("context pack");
        assert!(context
            .results
            .iter()
            .all(|result| result.source_id != outside_id_text));
        let result = context
            .results
            .iter()
            .find(|result| result.source_id == inside_id_text)
            .expect("inside collection result");
        assert_eq!(result.collections[0].name, "articles");
    }

    #[tokio::test]
    async fn ask_context_only_returns_context_pack_when_chat_is_disabled_and_unavailable() {
        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("ask-context-only-chat-disabled");
        let source_path = test_dir.path().join("doc.md");
        fs::write(
            &source_path,
            "Beta retrieval evidence answers the context-only ask question.",
        )
        .unwrap();
        let config = retrieve_test_config(&model_server.base_url);
        assert!(!config.chat.enabled);

        let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        pipeline.ingest_source(&source_id).await.unwrap();

        let state = test_state(config, test_dir.path(), pipeline);
        let response = ask(
            State(state),
            Json(AskRequest {
                question: "Beta context-only question?".into(),
                source_id: Some(source_id.0.clone()),
                collection_filter: CollectionFilterRequest::default(),
                embedding_profile_id: None,
                show_retrieval: false,
                context_only: true,
                limit: None,
                page_size: None,
                page: None,
            }),
        )
        .await
        .unwrap()
        .0;

        assert!(response.answer.is_empty());
        assert!(response.generated_interpretation.is_none());
        assert!(response.citations.is_empty());
        assert!(!response.verified);
        assert!(response.retrieval.is_none());
        let context = response.context.expect("context pack");
        assert_eq!(context.source_id.as_deref(), Some(source_id.0.as_str()));
        assert_eq!(context.returned_results, 1);
        assert!(context.source_bounded);
        assert_eq!(context.results[0].label, "E1");
        assert!(!context.results[0].text_hash.is_empty());
        assert!(context.results[0]
            .snippet
            .contains("Beta retrieval evidence"));
        assert!(context.results[0].structured_locator.is_some());
        assert!(model_server.embedding_requests() >= 2);
        assert_eq!(model_server.chat_requests(), 0);
    }

    #[tokio::test]
    async fn completed_generated_interpretation_ask_uses_persisted_state_without_chat_or_verifier_rerun(
    ) {
        let model_server =
            MockModelServer::start_with_chat(3, "BM25 answer from evidence [E1]").await;
        let test_dir = TestDir::new("completed-ask-profile-no-rerun");
        let source_path = test_dir.path().join("doc.md");
        fs::write(
            &source_path,
            "Ask profile lookup should reuse completed retrieval and generation state.",
        )
        .unwrap();
        let mut config = retrieve_test_config(&model_server.base_url);
        config.embedding.enabled = false;
        config.chat.enabled = true;
        config.chat.base_url = model_server.base_url.clone();
        config.chat.model = "test-chat".into();
        config.retrieval.dense_top_k = 11;
        config.retrieval.bm25_top_k = 7;
        config.rerank.enabled = false;
        config.rerank.top_n = 5;
        config.verifier.enabled = false;

        let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        pipeline.ingest_source(&source_id).await.unwrap();

        let state = test_state(config, test_dir.path(), pipeline);
        let response = ask(
            State(Arc::clone(&state)),
            Json(AskRequest {
                question: "What should ask profile lookup reuse?".into(),
                source_id: Some(source_id.0.clone()),
                collection_filter: CollectionFilterRequest::default(),
                embedding_profile_id: None,
                show_retrieval: false,
                context_only: false,
                limit: None,
                page_size: None,
                page: None,
            }),
        )
        .await
        .unwrap()
        .0;

        assert!(response.answer.contains("BM25 answer"));
        assert_eq!(
            response.generated_interpretation.unwrap().text,
            response.answer
        );
        assert!(response.retrieval.is_none());
        assert_eq!(model_server.embedding_requests(), 0);
        assert_eq!(model_server.chat_requests(), 1);
        let task_id = latest_task_id(&state, TaskKind::Ask);

        {
            let mut runtime = state.runtime_config.write().unwrap();
            runtime.config.retrieval.dense_top_k = 999;
            runtime.config.retrieval.bm25_top_k = 999;
            runtime.config.rerank.enabled = true;
            runtime.config.rerank.top_n = 99;
            runtime.config.qdrant.enabled = true;
            runtime.config.qdrant.prefer_for_search = true;
        }

        let before_embedding = model_server.embedding_requests();
        let before_chat = model_server.chat_requests();
        let before_models = model_server.model_requests();
        let Json(profile_response) =
            task_profile_handler(State(Arc::clone(&state)), Path(task_id.0.clone()))
                .await
                .unwrap();

        assert_eq!(profile_response.profile.task_id, task_id);
        assert_eq!(profile_response.profile.task_kind, TaskKind::Ask);
        assert_eq!(profile_response.profile.status, TaskStatus::Succeeded);
        let value = serde_json::to_value(&profile_response.profile).unwrap();
        assert_eq!(value["controls"]["retrieval"]["dense_top_k"], 11);
        assert_eq!(value["controls"]["retrieval"]["bm25_top_k"], 7);
        assert_eq!(value["controls"]["rerank"]["enabled"], false);
        assert_eq!(value["controls"]["rerank"]["configured_top_n"], 5);
        assert_eq!(
            value["controls"]["rerank"]["effective_top_n"],
            serde_json::Value::Null
        );
        assert_eq!(value["controls"]["qdrant"]["enabled"], false);
        assert_eq!(value["controls"]["vector"]["embedding_enabled"], false);
        assert_eq!(value["controls"]["vector"]["dense_path"], "bm25_only");
        assert_eq!(
            value["controls"]["filters"]["source"]["effective_source_count"],
            1
        );
        assert_eq!(value["controls"]["output"]["show_retrieval"], false);
        assert!(value["resources"]["queues"]
            .as_array()
            .unwrap()
            .iter()
            .any(|resource| resource["name"] == "sqlite_reader"
                && resource.get("latest_queue_wait_ms").is_some()));
        let retrieve_profile = profile_response
            .profile
            .retrieve
            .as_ref()
            .expect("ask profile should include retrieval stages");
        assert_eq!(
            retrieve_profile.dense.path,
            RetrievalDenseVectorPath::Bm25Only
        );
        assert!(retrieve_profile.bm25.candidate_count > 0);
        assert!(retrieve_profile.fusion.candidate_count > 0);
        let ask_profile = profile_response
            .profile
            .ask
            .as_ref()
            .expect("ask profile should include generation stages");
        assert_eq!(
            ask_profile.generation.status,
            AskGenerationStatus::Succeeded
        );
        assert_eq!(ask_profile.generation.call_count, 1);
        assert_eq!(ask_profile.generation.error_count, 0);
        assert!(!ask_profile.verification.enabled);
        assert_eq!(
            ask_profile.verification.status,
            AskVerificationStatus::Disabled
        );
        assert_eq!(ask_profile.verification.call_count, 0);
        assert_eq!(ask_profile.verification.latest_latency_ms, None);
        assert_eq!(ask_profile.output.citation_count, response.citations.len());
        assert!(!ask_profile.output.retrieval_included);
        assert!(profile_response
            .profile
            .endpoints
            .iter()
            .any(|endpoint| endpoint.name == "chat" && endpoint.calls == 1));

        let encoded = serde_json::to_string(&profile_response.profile).unwrap();
        assert!(encoded.len() <= 16 * 1024);
        assert!(!encoded.contains("BM25 answer"));
        assert!(!encoded.contains("What should ask profile lookup reuse?"));
        assert!(!encoded.contains("Ask profile lookup should reuse"));
        assert!(!encoded.contains("SOURCE PACK"));
        assert!(!encoded.contains("USER QUESTION"));
        assert!(!encoded.contains("bm25_hits"));
        assert!(!encoded.contains("dense_hits"));
        assert!(!encoded.contains("api_key"));
        assert_eq!(model_server.embedding_requests(), before_embedding);
        assert_eq!(model_server.chat_requests(), before_chat);
        assert_eq!(model_server.model_requests(), before_models);
    }

    #[tokio::test]
    async fn completed_ask_profile_records_enabled_verifier_calls_without_rerun() {
        let model_server = MockModelServer::start_with_chat_responses(
            3,
            [
                "Verified answer from evidence [E1]",
                r#"{"verdict":"pass","unsupported_claims":[]}"#,
            ],
        )
        .await;
        let test_dir = TestDir::new("completed-ask-profile-verifier");
        let source_path = test_dir.path().join("doc.md");
        fs::write(
            &source_path,
            "Verifier profile lookup should separate retrieval, generation, and verifier work.",
        )
        .unwrap();
        let mut config = retrieve_test_config(&model_server.base_url);
        config.embedding.enabled = false;
        config.chat.enabled = true;
        config.chat.base_url = model_server.base_url.clone();
        config.chat.model = "test-chat".into();
        config.rerank.enabled = false;
        config.verifier.enabled = true;

        let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        pipeline.ingest_source(&source_id).await.unwrap();

        let state = test_state(config, test_dir.path(), pipeline);
        let response = ask(
            State(Arc::clone(&state)),
            Json(AskRequest {
                question: "What should verifier profile lookup separate?".into(),
                source_id: Some(source_id.0.clone()),
                collection_filter: CollectionFilterRequest::default(),
                embedding_profile_id: None,
                show_retrieval: true,
                context_only: false,
                limit: None,
                page_size: None,
                page: None,
            }),
        )
        .await
        .unwrap()
        .0;

        assert!(response.verified);
        assert!(response.retrieval.is_some());
        assert_eq!(model_server.embedding_requests(), 0);
        assert_eq!(model_server.chat_requests(), 2);
        let task_id = latest_task_id(&state, TaskKind::Ask);

        let before_embedding = model_server.embedding_requests();
        let before_chat = model_server.chat_requests();
        let before_models = model_server.model_requests();
        let Json(profile_response) =
            task_profile_handler(State(Arc::clone(&state)), Path(task_id.0.clone()))
                .await
                .unwrap();

        let value = serde_json::to_value(&profile_response.profile).unwrap();
        assert_eq!(value["task_kind"], "ask");
        assert_eq!(value["retrieve"]["dense"]["path"], "bm25_only");
        assert_eq!(value["ask"]["generation"]["call_count"], 1);
        assert_eq!(value["ask"]["generation"]["status"], "succeeded");
        assert_eq!(value["ask"]["verification"]["enabled"], true);
        assert_eq!(value["ask"]["verification"]["status"], "passed");
        assert_eq!(value["ask"]["verification"]["call_count"], 1);
        assert_eq!(value["ask"]["verification"]["error_count"], 0);
        assert_eq!(value["ask"]["output"]["retrieval_included"], true);
        assert!(profile_response
            .profile
            .endpoints
            .iter()
            .any(|endpoint| endpoint.name == "chat" && endpoint.calls == 1));
        assert!(profile_response
            .profile
            .endpoints
            .iter()
            .any(|endpoint| endpoint.name == "verifier" && endpoint.calls == 1));
        assert_eq!(model_server.embedding_requests(), before_embedding);
        assert_eq!(model_server.chat_requests(), before_chat);
        assert_eq!(model_server.model_requests(), before_models);
    }

    #[tokio::test]
    async fn global_search_surfaces_only_store_backing_evidence_to_retrieve_and_ask() {
        let model_server = MockModelServer::start_with_chat_responses(
            3,
            [
                "The cedar protocol is backed by stored evidence [E1]",
                r#"{"verdict":"pass","unsupported_claims":[]}"#,
            ],
        )
        .await;
        let test_dir = TestDir::new("graphrag-global-search-ordinary-ask");
        let source_path = test_dir.path().join("cedar.md");
        fs::write(
            &source_path,
            "The cedar protocol is backed by this authoritative stored passage.",
        )
        .unwrap();
        let distractor_path = test_dir.path().join("distractor.md");
        fs::write(
            &distractor_path,
            "Zirconium provenance appears here as a lexical distractor only.",
        )
        .unwrap();
        let mut config = retrieve_test_config(&model_server.base_url);
        config.embedding.enabled = false;
        config.chat.enabled = true;
        config.chat.base_url = model_server.base_url.clone();
        config.chat.model = "test-chat".into();
        config.rerank.enabled = false;
        config.verifier.enabled = true;
        config.graph.global_search.enabled = true;

        let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        pipeline.ingest_source(&source_id).await.unwrap();
        let distractor_source_id = pipeline.add_source(&distractor_path).unwrap();
        pipeline.ingest_source(&distractor_source_id).await.unwrap();
        let chunk = pipeline
            .store()
            .list_chunks_by_source(&source_id)
            .unwrap()
            .into_iter()
            .find(|chunk| chunk.chunk_type == ChunkType::Child)
            .expect("ingest creates a child chunk");
        let graph_report_prose =
            "Generated graph claim selects the cedar passage for zirconium provenance.";
        let external_id = "generated_claim:cedar-provenance";
        let claim = GraphNode {
            id: GraphNodeId::new(&source_id, GraphNodeKind::GeneratedClaim, external_id),
            source_id: source_id.clone(),
            kind: GraphNodeKind::GeneratedClaim,
            external_id: external_id.into(),
            label: Some(graph_report_prose.into()),
            locator: None,
            ordinal: None,
            metadata: Some(serde_json::json!({
                "origin": "llm_generated",
                "graph_data_kind": "claim",
                "claim": graph_report_prose,
                "subject": "cedar passage",
                "predicate": "supports",
                "object": "zirconium provenance",
                "source_spans": [format!("{}:1-1", chunk.id.0)]
            })),
        };
        pipeline
            .store()
            .upsert_graph_nodes(std::slice::from_ref(&claim))
            .unwrap();
        let global_hits = verbatim_core::graphrag::GraphRagService::new(
            pipeline.store(),
            &config.graph.global_search,
        )
        .global_search("zirconium provenance", None)
        .unwrap();
        assert_eq!(global_hits.len(), 1, "fixture must contain a global report");
        let graph_evidence_id = chunk.evidence_unit_ids[0].clone();

        let state = test_state(config, test_dir.path(), pipeline);
        let retrieve_request = context_only_retrieve_request(AskRequest {
            question: "What supports zirconium provenance?".into(),
            source_id: None,
            collection_filter: CollectionFilterRequest::default(),
            embedding_profile_id: None,
            show_retrieval: true,
            context_only: true,
            limit: Some(3),
            page_size: Some(3),
            page: None,
        });
        let Json(retrieve_response) = retrieve(State(Arc::clone(&state)), Json(retrieve_request))
            .await
            .unwrap();
        assert_eq!(
            retrieve_response.results[0].evidence_id, graph_evidence_id.0,
            "global search must reprioritize its Store-backed evidence"
        );
        assert!(retrieve_response
            .results
            .iter()
            .any(|result| result.source_id == distractor_source_id.0));

        let response = ask(
            State(Arc::clone(&state)),
            Json(AskRequest {
                question: "What supports zirconium provenance?".into(),
                source_id: None,
                collection_filter: CollectionFilterRequest::default(),
                embedding_profile_id: None,
                show_retrieval: true,
                context_only: false,
                limit: None,
                page_size: None,
                page: None,
            }),
        )
        .await
        .unwrap()
        .0;

        assert!(response.verified);
        let retrieval = response
            .retrieval
            .as_ref()
            .expect("retrieval debug is published");
        assert!(!retrieval.final_evidence_pack.is_empty());
        assert!(!response.citations.is_empty());
        assert_eq!(
            retrieval.final_evidence_pack[0].evidence_id,
            graph_evidence_id
        );
        assert_eq!(response.citations[0].evidence_id, graph_evidence_id.0);
        let published_ids = retrieval
            .final_evidence_pack
            .iter()
            .map(|entry| entry.evidence_id.clone())
            .chain(
                retrieve_response
                    .results
                    .iter()
                    .map(|result| EvidenceId(result.evidence_id.clone())),
            )
            .chain(
                response
                    .citations
                    .iter()
                    .map(|citation| EvidenceId(citation.evidence_id.clone())),
            )
            .collect::<Vec<_>>();
        let pipeline = state.pipeline.lock().unwrap();
        let store = pipeline
            .as_ref()
            .expect("query pipeline is restored")
            .store();
        for evidence_id in &published_ids {
            assert!(!evidence_id.0.starts_with("graphrag:"));
            assert!(
                store.get_evidence(evidence_id).unwrap().is_some(),
                "published evidence must resolve through Store: {}",
                evidence_id.0
            );
        }
        drop(pipeline);

        let chat_payloads = model_server.chat_payloads();
        assert_eq!(chat_payloads.len(), 2);
        let source_pack_payload = serde_json::to_string(&chat_payloads[0]).unwrap();
        let verifier_payload = serde_json::to_string(&chat_payloads[1]).unwrap();
        assert!(source_pack_payload.contains("SOURCE PACK"));
        assert!(source_pack_payload.contains("authoritative stored passage"));
        assert!(verifier_payload.contains(&response.citations[0].evidence_id));
        assert!(!source_pack_payload.contains("graphrag:"));
        assert!(!verifier_payload.contains("graphrag:"));
        assert!(!source_pack_payload.contains(graph_report_prose));
        assert!(!verifier_payload.contains(graph_report_prose));
    }

    #[tokio::test]
    async fn completed_retrieve_profile_query_uses_persisted_state_without_model_calls() {
        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("completed-retrieve-profile-no-rerun");
        let source_path = test_dir.path().join("doc.md");
        fs::write(
            &source_path,
            "Profile lookup should reuse the completed retrieve task state.",
        )
        .unwrap();
        let mut config = retrieve_test_config(&model_server.base_url);
        config.embedding.enabled = false;
        config.rerank.enabled = false;

        let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        pipeline.ingest_source(&source_id).await.unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let Json(retrieve_response) = retrieve(
            State(Arc::clone(&state)),
            Json(RetrieveRequest {
                question: "Profile lookup retrieve task state?".into(),
                source_id: Some(source_id.0),
                collection_filter: CollectionFilterRequest::default(),
                embedding_profile_id: None,
                limit: Some(3),
                page_size: Some(3),
                page: Some(1),
                fast: true,
                rerank: Some(false),
                dense_top_k: None,
                bm25_top_k: Some(3),
                rerank_top_n: None,
                bypass_cache: false,
                include_debug: true,
                include_debug_packs: false,
                include_locator: false,
                passage: false,
            }),
        )
        .await
        .unwrap();

        {
            let mut runtime = state.runtime_config.write().unwrap();
            runtime.config.retrieval.dense_top_k = 777;
            runtime.config.retrieval.bm25_top_k = 777;
            runtime.config.rerank.enabled = true;
            runtime.config.rerank.top_n = 77;
            runtime.config.qdrant.enabled = true;
            runtime.config.qdrant.prefer_for_search = true;
        }

        let before_embedding = model_server.embedding_requests();
        let before_chat = model_server.chat_requests();
        let before_models = model_server.model_requests();
        let Json(profile_response) = task_profile_handler(
            State(Arc::clone(&state)),
            Path(retrieve_response.task_id.clone()),
        )
        .await
        .unwrap();

        assert_eq!(
            profile_response.profile.task_id.0,
            retrieve_response.task_id
        );
        assert_eq!(profile_response.profile.task_kind, TaskKind::Retrieve);
        assert_eq!(profile_response.profile.status, TaskStatus::Succeeded);
        assert_eq!(
            profile_response.profile.schema_version,
            verbatim_core::task::TASK_PROFILE_SCHEMA_VERSION
        );
        let value = serde_json::to_value(&profile_response.profile).unwrap();
        assert_eq!(value["controls"]["retrieval"]["dense_top_k"], 20);
        assert_eq!(value["controls"]["retrieval"]["bm25_top_k"], 3);
        assert_eq!(value["controls"]["retrieval"]["fast"], true);
        assert_eq!(value["controls"]["rerank"]["enabled"], false);
        assert_eq!(value["controls"]["rerank"]["configured_top_n"], 0);
        assert_eq!(
            value["controls"]["rerank"]["effective_top_n"],
            serde_json::Value::Null
        );
        assert_eq!(value["controls"]["qdrant"]["enabled"], false);
        assert_eq!(value["controls"]["qdrant"]["preferred"], false);
        assert_eq!(value["controls"]["qdrant"]["used"], false);
        assert_eq!(value["controls"]["vector"]["embedding_enabled"], false);
        assert_eq!(value["controls"]["vector"]["dense_path"], "bm25_only");
        assert_eq!(
            value["controls"]["filters"]["source"]["effective_source_count"],
            1
        );
        assert_eq!(value["controls"]["output"]["limit"], 3);
        assert_eq!(value["controls"]["output"]["page_size"], 3);
        assert_eq!(value["controls"]["output"]["page"], 1);
        assert_eq!(
            value["resources"]["queues"]
                .as_array()
                .unwrap()
                .iter()
                .filter(|resource| resource["name"] == "sqlite_reader")
                .count(),
            1
        );
        assert!(
            value["resources"]["queues"]
                .as_array()
                .unwrap()
                .iter()
                .any(|resource| resource["name"] == "cpu_worker"
                    && resource.get("latest_service_ms").is_some()),
            "profile should serialize unavailable resource timings as stable null fields"
        );
        assert!(profile_response.profile.endpoints.is_empty());
        let retrieve_profile = profile_response
            .profile
            .retrieve
            .as_ref()
            .expect("retrieve task profile should include retrieve stages");
        assert_eq!(
            retrieve_profile.dense.path,
            RetrievalDenseVectorPath::Bm25Only
        );
        assert_eq!(retrieve_profile.dense.candidate_count, 0);
        assert!(retrieve_profile.bm25.candidate_count > 0);
        assert!(retrieve_profile.fusion.candidate_count > 0);
        assert!(retrieve_profile.evidence.final_count > 0);
        assert_eq!(
            retrieve_profile.display.returned_count,
            retrieve_response.returned_results
        );
        assert!(profile_response.profile.total_wall_ms >= profile_response.profile.queue_wait_ms);
        let encoded = serde_json::to_string(&profile_response.profile).unwrap();
        assert!(encoded.len() <= 16 * 1024);
        assert!(!encoded.contains("Profile lookup should reuse"));
        assert!(!encoded.contains("bm25_hits"));
        assert!(!encoded.contains("dense_hits"));
        assert!(!encoded.contains("final_evidence_pack"));
        assert_eq!(model_server.embedding_requests(), before_embedding);
        assert_eq!(model_server.chat_requests(), before_chat);
        assert_eq!(model_server.model_requests(), before_models);
    }

    #[tokio::test]
    async fn retrieve_profile_json_distinguishes_slow_local_work_from_fast_model_endpoints() {
        let test_dir = TestDir::new("retrieve-profile-slow-local-fast-model");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let task_id = create_persisted_task(
            &state,
            TaskKind::Retrieve,
            retrieve_request_metadata("Where did retrieve spend time?", None, None, 20, 20, 20),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &task_id).await.unwrap();

        let profile = TaskProfile {
            schema_version: verbatim_core::task::TASK_PROFILE_SCHEMA_VERSION,
            task_id: task_id.clone(),
            task_kind: TaskKind::Retrieve,
            status: TaskStatus::Succeeded,
            queue_wait_ms: 7,
            total_wall_ms: 136_572,
            controls: Default::default(),
            resources: Default::default(),
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
        finish_task_success_with_profile(
            &state,
            &task_id,
            retrieve_result_metadata(100, 10, true),
            profile,
        )
        .await
        .unwrap();

        let Json(profile_response) =
            task_profile_handler(State(Arc::clone(&state)), Path(task_id.0.clone()))
                .await
                .unwrap();
        let value = serde_json::to_value(&profile_response.profile).unwrap();

        assert_eq!(value["endpoints"][0]["latest_latency_ms"], 761);
        assert_eq!(value["endpoints"][1]["latest_latency_ms"], 1_357);
        assert_eq!(value["retrieve"]["dense"]["path"], "low_memory_sqlite_scan");
        assert_eq!(value["retrieve"]["dense"]["local_ms"], 96_000);
        assert_eq!(value["retrieve"]["evidence"]["result_hydration_ms"], 21_000);
        assert_eq!(value["retrieve"]["evidence"]["display_pack_ms"], 9_500);
        assert_eq!(
            value["retrieve"]["rerank"]["endpoint_latency_ms"],
            serde_json::json!(1_357)
        );
        let local_ms = value["retrieve"]["dense"]["local_ms"].as_u64().unwrap()
            + value["retrieve"]["evidence"]["result_hydration_ms"]
                .as_u64()
                .unwrap()
            + value["retrieve"]["evidence"]["display_pack_ms"]
                .as_u64()
                .unwrap();
        let model_ms = value["endpoints"]
            .as_array()
            .unwrap()
            .iter()
            .map(|endpoint| endpoint["latest_latency_ms"].as_u64().unwrap())
            .sum::<u64>();
        assert!(local_ms > model_ms * 30);
    }

    #[tokio::test]
    async fn task_profile_api_reports_clear_unavailable_error_cases() {
        let test_dir = TestDir::new("task-profile-unavailable-errors");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let missing = task_profile_handler(State(Arc::clone(&state)), Path("missing".into()))
            .await
            .unwrap_err();
        let (status, Json(body)) = missing;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body.error.as_ref(), "task not found: missing");

        let queued_id = create_persisted_task(
            &state,
            TaskKind::Retrieve,
            retrieve_request_metadata("queued task", None, None, 3, 3, 1),
        )
        .await
        .unwrap();
        let queued = task_profile_handler(State(Arc::clone(&state)), Path(queued_id.0.clone()))
            .await
            .unwrap_err();
        let (status, Json(body)) = queued;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body
            .error
            .contains("task profile unavailable for incomplete task"));
        assert!(body.error.contains("status queued"));

        let ingest_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, false),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &ingest_id).await.unwrap();
        finish_task_success(&state, &ingest_id, serde_json::json!({"ingested": 0}))
            .await
            .unwrap();
        let unsupported =
            task_profile_handler(State(Arc::clone(&state)), Path(ingest_id.0.clone()))
                .await
                .unwrap_err();
        let (status, Json(body)) = unsupported;
        assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
        assert_eq!(
            body.error.as_ref(),
            format!("task profile unsupported for ingest task: {}", ingest_id.0)
        );

        let legacy_id = create_persisted_task(
            &state,
            TaskKind::Retrieve,
            retrieve_request_metadata("legacy task", None, None, 3, 3, 1),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &legacy_id).await.unwrap();
        finish_task_success(&state, &legacy_id, retrieve_result_metadata(0, 0, false))
            .await
            .unwrap();
        let legacy = task_profile_handler(State(Arc::clone(&state)), Path(legacy_id.0.clone()))
            .await
            .unwrap_err();
        let (status, Json(body)) = legacy;
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body.error.as_ref(),
            format!(
                "task profile unavailable for legacy/no-profile task: {}",
                legacy_id.0
            )
        );

        let corrupt_id = create_persisted_task(
            &state,
            TaskKind::Retrieve,
            retrieve_request_metadata("corrupt task", None, None, 3, 3, 1),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &corrupt_id).await.unwrap();
        finish_task_success(&state, &corrupt_id, retrieve_result_metadata(0, 0, false))
            .await
            .unwrap();
        let conn = rusqlite::Connection::open(test_dir.path().join("verbatim.db")).unwrap();
        conn.execute(
            "UPDATE tasks SET profile_json = ?2 WHERE id = ?1",
            rusqlite::params![&corrupt_id.0, "{not valid json"],
        )
        .unwrap();
        drop(conn);

        let corrupt = task_profile_handler(State(Arc::clone(&state)), Path(corrupt_id.0.clone()))
            .await
            .unwrap_err();
        let (status, Json(body)) = corrupt;
        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
        assert!(body.error.contains(&format!(
            "stored task profile JSON is malformed for task: {}",
            corrupt_id.0
        )));
    }

    #[tokio::test]
    async fn task_profile_api_hides_partial_profile_for_running_task() {
        let test_dir = TestDir::new("task-profile-running-partial");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let task_id = create_persisted_task(
            &state,
            TaskKind::Retrieve,
            retrieve_request_metadata("running task", None, None, 3, 3, 1),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &task_id).await.unwrap();

        let profile = minimal_task_profile(&task_id, TaskKind::Retrieve, TaskStatus::Succeeded);
        let conn = rusqlite::Connection::open(test_dir.path().join("verbatim.db")).unwrap();
        conn.execute(
            "UPDATE tasks SET profile_json = ?2 WHERE id = ?1",
            rusqlite::params![&task_id.0, serde_json::to_string(&profile).unwrap()],
        )
        .unwrap();
        drop(conn);

        let error = task_profile_handler(State(Arc::clone(&state)), Path(task_id.0.clone()))
            .await
            .unwrap_err();
        let (status, Json(body)) = error;
        assert_eq!(status, StatusCode::CONFLICT);
        assert_eq!(
            body.error.as_ref(),
            format!(
                "task profile unavailable for incomplete task {} (status running)",
                task_id.0
            )
        );
    }

    #[tokio::test]
    async fn failed_and_cancelled_profile_tasks_without_profile_are_unavailable() {
        let test_dir = TestDir::new("task-profile-terminal-unavailable");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let failed_id = create_persisted_task(
            &state,
            TaskKind::Retrieve,
            retrieve_request_metadata("failed task", None, None, 3, 3, 1),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &failed_id).await.unwrap();
        with_task_store_write(&state, {
            let failed_id = failed_id.clone();
            move |store| store.finish_task_failed(&failed_id, "retrieval failed")
        })
        .await
        .unwrap();

        let cancelled_id = create_persisted_task(
            &state,
            TaskKind::Ask,
            ask_request_metadata("cancelled task", None, None, false, false),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &cancelled_id).await.unwrap();
        cancel_task_record(&state, &cancelled_id).await.unwrap();

        for task_id in [&failed_id, &cancelled_id] {
            let error = task_profile_handler(State(Arc::clone(&state)), Path(task_id.0.clone()))
                .await
                .unwrap_err();
            let (status, Json(body)) = error;
            assert_eq!(status, StatusCode::NOT_FOUND);
            assert_eq!(
                body.error.as_ref(),
                format!(
                    "task profile unavailable for legacy/no-profile task: {}",
                    task_id.0
                )
            );
        }
    }

    #[tokio::test]
    async fn failed_task_profile_is_returned_when_persisted() {
        let test_dir = TestDir::new("task-profile-failed-persisted");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let task_id = create_persisted_task(
            &state,
            TaskKind::Retrieve,
            retrieve_request_metadata("failed with profile", None, None, 3, 3, 1),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &task_id).await.unwrap();

        let profile = minimal_task_profile(&task_id, TaskKind::Retrieve, TaskStatus::Failed);
        with_task_store_write(&state, {
            let task_id = task_id.clone();
            move |store| store.finish_task_failed(&task_id, "retrieval failed")
        })
        .await
        .unwrap();
        let conn = rusqlite::Connection::open(test_dir.path().join("verbatim.db")).unwrap();
        conn.execute(
            "UPDATE tasks SET profile_json = ?2 WHERE id = ?1",
            rusqlite::params![&task_id.0, serde_json::to_string(&profile).unwrap()],
        )
        .unwrap();
        drop(conn);

        let Json(response) = task_profile_handler(State(Arc::clone(&state)), Path(task_id.0))
            .await
            .unwrap();
        assert_eq!(response.profile, profile);
        assert_eq!(response.profile.status, TaskStatus::Failed);
    }

    #[tokio::test]
    async fn running_profile_query_is_bounded_and_completion_later_returns_persisted_profile() {
        let test_dir = TestDir::new("task-profile-running-then-complete");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let task_id = create_persisted_task(
            &state,
            TaskKind::Retrieve,
            retrieve_request_metadata("interleaved task", None, None, 3, 3, 1),
        )
        .await
        .unwrap();
        ensure_task_started(&state, &task_id).await.unwrap();

        let writer_permit = state
            .resources
            .sqlite_writer
            .acquire()
            .await
            .expect("writer permit");
        let running = tokio::time::timeout(
            Duration::from_millis(250),
            task_profile_handler(State(Arc::clone(&state)), Path(task_id.0.clone())),
        )
        .await
        .expect("running profile query should not wait behind writer resource")
        .unwrap_err();
        let (status, Json(body)) = running;
        assert_eq!(status, StatusCode::CONFLICT);
        assert!(body.error.contains("status running"));
        drop(writer_permit);

        let profile = minimal_task_profile(&task_id, TaskKind::Retrieve, TaskStatus::Succeeded);
        finish_task_success_with_profile(
            &state,
            &task_id,
            retrieve_result_metadata(0, 0, false),
            profile.clone(),
        )
        .await
        .unwrap();

        let Json(completed) =
            task_profile_handler(State(Arc::clone(&state)), Path(task_id.0.clone()))
                .await
                .unwrap();
        assert_eq!(completed.profile, profile);
    }

    #[tokio::test]
    async fn explicit_llm_rerank_out_of_range_output_falls_back_without_embedding_calls() {
        let (response, model_server) = retrieve_with_llm_rerank_chat_response(
            "retrieve-llm-rerank-invalid",
            r#"{"rankings":[{"index":99,"score":0.9}]}"#,
        )
        .await;

        assert_eq!(response.returned_results, 1);
        let debug = response.debug.expect("retrieval debug");
        assert_eq!(debug.reranker.status, RetrievalRerankStatus::Fallback);
        assert_eq!(debug.reranker.reason.as_deref(), Some("invalid_response"));
        assert_eq!(debug.query_embedding_latency_ms, None);
        assert!(debug.dense_hits.is_empty());
        assert_eq!(model_server.embedding_requests(), 0);
        assert_eq!(model_server.chat_requests(), 1);
    }

    #[tokio::test]
    async fn explicit_llm_rerank_mixed_invalid_output_falls_back_without_partial_success() {
        let (response, model_server) = retrieve_with_llm_rerank_chat_response(
            "retrieve-llm-rerank-mixed-invalid",
            r#"{"rankings":[{"index":99,"score":0.9},{"index":0,"score":0.1}]}"#,
        )
        .await;

        assert_eq!(response.returned_results, 1);
        let debug = response.debug.expect("retrieval debug");
        assert_eq!(debug.reranker.status, RetrievalRerankStatus::Fallback);
        assert_eq!(debug.reranker.reason.as_deref(), Some("invalid_response"));
        assert_eq!(debug.query_embedding_latency_ms, None);
        assert!(debug.dense_hits.is_empty());
        assert_eq!(model_server.embedding_requests(), 0);
        assert_eq!(model_server.chat_requests(), 1);
    }

    async fn retrieve_with_llm_rerank_chat_response(
        test_name: &str,
        chat_response: &str,
    ) -> (RetrieveResponse, MockModelServer) {
        let model_server = MockModelServer::start_with_chat(3, chat_response).await;
        let test_dir = TestDir::new(test_name);
        let source_path = test_dir.path().join("doc.md");
        fs::write(
            &source_path,
            "Alpha LLM rerank fallback evidence should still be returned.",
        )
        .unwrap();
        let mut config = retrieve_test_config(&model_server.base_url);
        config.embedding.enabled = false;
        config.rerank.enabled = true;
        config.rerank.strategy = RerankStrategy::Llm;
        config.rerank.base_url = model_server.base_url.clone();
        config.rerank.model = "test-llm-reranker".into();
        config.rerank.top_n = 1;

        let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        pipeline.ingest_source(&source_id).await.unwrap();

        let state = test_state(config, test_dir.path(), pipeline);
        let response = retrieve(
            State(state),
            Json(RetrieveRequest {
                question: "Alpha fallback evidence?".into(),
                source_id: Some(source_id.0.clone()),
                collection_filter: CollectionFilterRequest::default(),
                embedding_profile_id: None,
                limit: Some(3),
                page_size: Some(1),
                page: Some(1),
                fast: false,
                rerank: None,
                dense_top_k: None,
                bm25_top_k: None,
                rerank_top_n: None,
                bypass_cache: false,
                include_debug: true,
                include_debug_packs: false,
                include_locator: false,
                passage: false,
            }),
        )
        .await
        .unwrap()
        .0;

        (response, model_server)
    }

    #[tokio::test]
    async fn reindex_one_source_preserves_canonical_source_record() {
        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("reindex-one-source");
        let source_path = test_dir.path().join("doc.txt");
        fs::write(&source_path, "alpha beta evidence").unwrap();
        let config = retrieve_test_config(&model_server.base_url);
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let Json(response) = reindex(
            State(Arc::clone(&state)),
            Json(ReindexRequest {
                source_id: Some(source_id.0.clone()),
                all: false,
                stale: false,
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.reindexed, 1);
        let sources = state
            .pipeline
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .store()
            .list_sources()
            .unwrap();
        assert_eq!(sources.len(), 1);
        assert_eq!(sources[0].id, source_id);
        assert_eq!(model_server.embedding_requests(), 1);
    }

    #[tokio::test]
    async fn reindex_force_without_target_reindexes_all_sources() {
        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("reindex-force-all-source");
        let source_path = test_dir.path().join("doc.txt");
        fs::write(&source_path, "alpha beta evidence").unwrap();
        let config = retrieve_test_config(&model_server.base_url);
        let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        pipeline.ingest_source(&source_id).await.unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let Json(response) = reindex(
            State(Arc::clone(&state)),
            Json(ReindexRequest {
                source_id: None,
                all: false,
                stale: false,
                force: true,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.reindexed, 1);
        assert_eq!(model_server.embedding_requests(), 1);
    }

    #[tokio::test]
    async fn vector_only_reindex_and_ingest_report_source_count_for_multi_chunk_source() {
        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("vector-only-counts-sources");
        let source_path = test_dir.path().join("doc.txt");
        let body = (0..8)
            .map(|index| {
                format!(
                    "Paragraph {index}: {}\n\n",
                    "alpha beta gamma delta ".repeat(50)
                )
            })
            .collect::<String>();
        fs::write(&source_path, body).unwrap();
        let config = retrieve_test_config(&model_server.base_url);
        let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        pipeline.ingest_source(&source_id).await.unwrap();
        let child_count = pipeline
            .store()
            .list_child_chunks()
            .unwrap()
            .into_iter()
            .filter(|chunk| chunk.source_id == source_id)
            .count();
        assert!(
            child_count > 1,
            "test fixture must create multiple child chunks"
        );
        let state = test_state(config, test_dir.path(), pipeline);

        let Json(reindex_response) = reindex(
            State(Arc::clone(&state)),
            Json(ReindexRequest {
                source_id: Some(source_id.0.clone()),
                all: false,
                stale: false,
                force: false,
                embedding_profile_id: Some("alt-reindex".into()),
                vectors_only: true,
            }),
        )
        .await
        .unwrap();
        let Json(ingest_response) = ingest_one(
            State(Arc::clone(&state)),
            Path(source_id.0.clone()),
            Query(IngestQuery {
                force: false,
                embedding_profile_id: Some("alt-ingest".into()),
                vectors_only: true,
            }),
        )
        .await
        .unwrap();

        assert_eq!(reindex_response.reindexed, 1);
        assert_eq!(ingest_response.ingested, 1);
        let pipeline = state.pipeline.lock().unwrap();
        let pipeline = pipeline.as_ref().unwrap();
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(
                    &EmbeddingProfileId::new("alt-reindex").unwrap(),
                    Some(&source_id),
                )
                .unwrap(),
            child_count
        );
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(
                    &EmbeddingProfileId::new("alt-ingest").unwrap(),
                    Some(&source_id),
                )
                .unwrap(),
            child_count
        );
    }

    #[tokio::test]
    async fn reindex_missing_source_returns_not_found() {
        let test_dir = TestDir::new("reindex-missing-source");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let (status, Json(body)) = reindex(
            State(state),
            Json(ReindexRequest {
                source_id: Some("__missing_source_smoke_retest__".into()),
                all: false,
                stale: false,
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            body.error.as_ref(),
            "source not found: __missing_source_smoke_retest__"
        );
    }

    #[tokio::test]
    async fn index_gc_dry_run_and_apply_use_daemon_data_dir() {
        let test_dir = TestDir::new("index-gc");
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.index_gc.retain_previous_generations = 0;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let profile = EmbeddingProfileId::default_profile();
        pipeline
            .store()
            .replace_all_vector_documents_for_profile(&profile, &[])
            .unwrap();
        let old_generation_number = pipeline
            .store()
            .index_generation_for_profile(&profile)
            .unwrap();
        let old_generation = test_dir
            .path()
            .join("indexes")
            .join("profiles")
            .join("default")
            .join(format!("gen-{old_generation_number}"));
        fs::create_dir_all(&old_generation).unwrap();
        fs::write(old_generation.join("vectors.hnsw"), b"old").unwrap();
        pipeline
            .store()
            .replace_all_vector_documents_for_profile(&profile, &[])
            .unwrap();
        let current_generation_number = pipeline
            .store()
            .index_generation_for_profile(&profile)
            .unwrap();
        let current_generation = test_dir
            .path()
            .join("indexes")
            .join("profiles")
            .join("default")
            .join(format!("gen-{current_generation_number}"));
        fs::create_dir_all(&current_generation).unwrap();
        fs::write(current_generation.join("vectors.hnsw"), b"current").unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let Json(dry_run) = index_gc(
            State(Arc::clone(&state)),
            Json(IndexGcRequest { dry_run: true }),
        )
        .await
        .unwrap();
        assert_eq!(dry_run.plan.entries.len(), 1);
        assert!(old_generation.exists());

        let Json(applied) = index_gc(
            State(Arc::clone(&state)),
            Json(IndexGcRequest { dry_run: false }),
        )
        .await
        .unwrap();
        assert_eq!(applied.apply.removed.len(), 1);
        assert!(!old_generation.exists());
        assert!(current_generation.exists());
    }

    #[tokio::test]
    async fn vector_json_cleanup_dry_run_and_apply_use_daemon_store() {
        let test_dir = TestDir::new("vector-json-cleanup");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let db_path = test_dir.path().join("verbatim.db");
        let state = test_state(config, test_dir.path(), pipeline);
        {
            let conn = rusqlite::Connection::open(&db_path).unwrap();
            conn.execute(
                "INSERT INTO sources (id, path, hash, status, parser_used, last_ingested_at)
                 VALUES ('source-1', '/tmp/vector-json-cleanup.md', 'hash', 'Indexed', 'test', NULL)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO chunks (id, source_id, chunk_hash, embedding_input_hash, text, context_text, token_count, chunk_type, parent_chunk_id, heading_path_json)
                 VALUES ('chunk-eligible', 'source-1', 'hash-eligible', 'input-eligible', 'text', NULL, 1, 'Leaf', NULL, '[]')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO chunks (id, source_id, chunk_hash, embedding_input_hash, text, context_text, token_count, chunk_type, parent_chunk_id, heading_path_json)
                 VALUES ('chunk-json-only', 'source-1', 'hash-json-only', 'input-json-only', 'text', NULL, 1, 'Leaf', NULL, '[]')",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO chunk_vectors (profile_id, chunk_id, source_id, vector_json, vector_blob)
                 VALUES ('default', 'chunk-eligible', 'source-1', '[1.0,2.0]', ?1)",
                [vec![0_u8; 8]],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO chunk_vectors (profile_id, chunk_id, source_id, vector_json, vector_blob)
                 VALUES ('default', 'chunk-json-only', 'source-1', '[3.0,4.0]', NULL)",
                [],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO embedding_cache
                    (profile_id, profile_config_hash, embedding_input_hash, vector_json, vector_blob, dimension, cache_hits, created_at, updated_at)
                 VALUES ('default', 'config-a', 'cache-eligible', '[1.0,2.0]', ?1, 2, 0, '1', '1')",
                [vec![0_u8; 8]],
            )
            .unwrap();
            conn.execute(
                "INSERT INTO embedding_cache
                    (profile_id, profile_config_hash, embedding_input_hash, vector_json, vector_blob, dimension, cache_hits, created_at, updated_at)
                 VALUES ('default', 'config-a', 'cache-json-only', '[3.0,4.0]', NULL, 2, 0, '1', '1')",
                [],
            )
            .unwrap();
        }

        let Json(dry_run) = vector_json_cleanup(
            State(Arc::clone(&state)),
            Json(VectorJsonCleanupRequest {
                dry_run: true,
                confirm: false,
            }),
        )
        .await
        .unwrap();
        assert!(dry_run.dry_run);
        assert_eq!(dry_run.report.tables.chunk_vectors.eligible, 1);
        assert_eq!(dry_run.report.tables.chunk_vectors.json_only, 1);
        assert_eq!(dry_run.report.tables.embedding_cache.eligible, 1);
        assert_eq!(dry_run.report.tables.embedding_cache.json_only, 1);
        assert_eq!(dry_run.report.cleared.chunk_vectors, 0);

        let Json(applied) = vector_json_cleanup(
            State(Arc::clone(&state)),
            Json(VectorJsonCleanupRequest {
                dry_run: false,
                confirm: true,
            }),
        )
        .await
        .unwrap();
        assert!(!applied.dry_run);
        assert_eq!(applied.report.cleared.chunk_vectors, 1);
        assert_eq!(applied.report.cleared.embedding_cache, 1);

        let conn = rusqlite::Connection::open(&db_path).unwrap();
        let chunk_eligible: String = conn
            .query_row(
                "SELECT vector_json FROM chunk_vectors WHERE chunk_id = 'chunk-eligible'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let chunk_json_only: String = conn
            .query_row(
                "SELECT vector_json FROM chunk_vectors WHERE chunk_id = 'chunk-json-only'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let cache_eligible: String = conn
            .query_row(
                "SELECT vector_json FROM embedding_cache WHERE embedding_input_hash = 'cache-eligible'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let cache_json_only: String = conn
            .query_row(
                "SELECT vector_json FROM embedding_cache WHERE embedding_input_hash = 'cache-json-only'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(chunk_eligible, "");
        assert_eq!(cache_eligible, "");
        assert_eq!(chunk_json_only, "[3.0,4.0]");
        assert_eq!(cache_json_only, "[3.0,4.0]");
    }

    #[tokio::test]
    async fn index_delete_profile_dry_run_and_apply_use_daemon_data_dir() {
        let test_dir = TestDir::new("index-delete-profile");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let profile = EmbeddingProfileId::new("old-profile").unwrap();
        pipeline
            .store()
            .ensure_embedding_profile(
                &profile,
                verbatim_core::store::EmbeddingProfileConfig {
                    provider: "test",
                    model: "old-model",
                    dimension: 2,
                    normalize: true,
                    endpoint_identity: None,
                    requested_model: None,
                    served_model: None,
                    max_context_tokens: None,
                    dtype: None,
                    quantization: None,
                    weight_identity: None,
                    chunker_version: "parent-child-v2",
                    child_target_tokens: 300,
                    child_overlap_tokens: 80,
                    parent_children_count: 5,
                    canonical_chunker_version:
                        verbatim_core::canonical_chunker::CANONICAL_CHUNKER_VERSION,
                    canonical_target_tokens: 300,
                    canonical_overlap_units: 2,
                    canonical_max_units_per_child: 20,
                    embedding_input_budget_tokens: None,
                    query_instruction: "",
                    document_instruction: "",
                },
            )
            .unwrap();
        pipeline
            .store()
            .replace_all_vector_documents_for_profile(&profile, &[])
            .unwrap();
        let profile_root = test_dir
            .path()
            .join("indexes")
            .join("profiles")
            .join("old-profile");
        fs::create_dir_all(profile_root.join("gen-1")).unwrap();
        fs::write(profile_root.join("gen-1").join("vectors.hnsw"), b"old").unwrap();
        let state = test_state(config, test_dir.path(), pipeline);

        let Json(dry_run) = index_delete_profile(
            State(Arc::clone(&state)),
            Json(IndexProfileDeleteRequest {
                profile_id: "old-profile".into(),
                dry_run: true,
                confirm: false,
                allow_active: false,
            }),
        )
        .await
        .unwrap();
        assert!(dry_run.plan.artifact.is_some());
        assert!(profile_root.exists());

        let Json(applied) = index_delete_profile(
            State(Arc::clone(&state)),
            Json(IndexProfileDeleteRequest {
                profile_id: "old-profile".into(),
                dry_run: false,
                confirm: true,
                allow_active: false,
            }),
        )
        .await
        .unwrap();
        assert_eq!(applied.apply.removed_artifacts.len(), 1);
        assert_eq!(applied.apply.sqlite.embedding_profile_index_meta_entries, 1);
        assert!(!profile_root.exists());
        let counts = state
            .pipeline
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .store()
            .embedding_profile_storage_counts(&profile)
            .unwrap();
        assert_eq!(counts.embedding_profile_index_meta_entries, 0);
    }

    #[tokio::test]
    async fn background_reindex_task_can_be_cancelled_while_queued() {
        let test_dir = TestDir::new("reindex-background-cancel");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let running_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, false),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(&state, &running_id)
            .await
            .unwrap();

        let Json(created) = submit_reindex_task(
            State(Arc::clone(&state)),
            Json(ReindexRequest {
                source_id: None,
                all: true,
                stale: false,
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap();
        let task_id = TaskId(created.task_id);

        let queued = task_summary_response(&state, task_id.clone())
            .await
            .unwrap()
            .task;
        assert_eq!(queued.status, TaskStatus::Queued);
        assert_eq!(queued.request["operation"], "reindex");

        let Json(cancelled) =
            cancel_task_handler(State(Arc::clone(&state)), Path(task_id.clone().0))
                .await
                .unwrap();

        assert_eq!(cancelled.task.status, TaskStatus::Cancelled);
    }

    #[tokio::test]
    async fn background_reindex_force_without_target_enqueues_all_source_reindex() {
        let test_dir = TestDir::new("reindex-background-force");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let running_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, false),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(&state, &running_id)
            .await
            .unwrap();

        let Json(created) = submit_reindex_task(
            State(Arc::clone(&state)),
            Json(ReindexRequest {
                source_id: None,
                all: false,
                stale: false,
                force: true,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap();
        let task_id = TaskId(created.task_id);

        let queued = task_summary_response(&state, task_id).await.unwrap().task;
        assert_eq!(queued.status, TaskStatus::Queued);
        assert_eq!(queued.request["operation"], "reindex");
        assert_eq!(queued.request["force"], true);
        assert!(queued.request["source_id"].is_null());
    }

    #[tokio::test]
    async fn cancelling_ingest_batch_parent_cancels_queued_children() {
        let test_dir = TestDir::new("ingest-batch-cancel-children");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let parent_id = TaskId::new();
        create_persisted_task_with_id(
            &state,
            parent_id.clone(),
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim_and_batch(
                None,
                false,
                None,
                false,
                false,
                Some(&parent_id.0),
            ),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(&state, &parent_id)
            .await
            .unwrap();
        let child_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim_and_batch(
                Some("src-child"),
                false,
                None,
                false,
                true,
                Some(&parent_id.0),
            ),
        )
        .await
        .unwrap();

        let Json(cancelled) =
            cancel_task_handler(State(Arc::clone(&state)), Path(parent_id.clone().0))
                .await
                .unwrap();

        assert_eq!(cancelled.task.status, TaskStatus::Cancelled);
        let child = task_summary_response(&state, child_id.clone())
            .await
            .unwrap()
            .task;
        assert_eq!(child.status, TaskStatus::Cancelled);
        let events = task_events_response(&state, parent_id, None, Some(10))
            .await
            .unwrap()
            .events;
        assert!(events
            .iter()
            .any(|event| event.event_type == "batch_cancelled"));
    }

    #[tokio::test]
    async fn cancelled_ingest_batch_children_are_not_claimed_after_restart() {
        let test_dir = TestDir::new("ingest-batch-restart-skip");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let parent_id = TaskId::new();
        create_persisted_task_with_id(
            &state,
            parent_id.clone(),
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim_and_batch(
                None,
                false,
                None,
                false,
                true,
                Some(&parent_id.0),
            ),
        )
        .await
        .unwrap();
        let child_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim_and_batch(
                Some("src-after-restart"),
                false,
                None,
                false,
                true,
                Some(&parent_id.0),
            ),
        )
        .await
        .unwrap();
        {
            let store = state.task_store.lock().unwrap();
            assert!(store.cancel_task(&parent_id).unwrap());
        }

        let claimed = claim_startable_ingest_task(&state).await.unwrap();

        assert!(claimed.is_none());
        let child = task_summary_response(&state, child_id).await.unwrap().task;
        assert_eq!(child.status, TaskStatus::Cancelled);
    }

    #[tokio::test]
    async fn all_source_background_ingest_submit_returns_while_pipeline_locked() {
        let test_dir = TestDir::new("ingest-batch-submit-immediate");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let state_for_lock = Arc::clone(&state);
        let lock_handle = tokio::task::spawn_blocking(move || {
            let _pipeline = state_for_lock.pipeline.lock().unwrap();
            let _ = locked_tx.send(());
            release_rx.recv().unwrap();
        });
        locked_rx.await.unwrap();

        let submitted = tokio::time::timeout(
            Duration::from_secs(1),
            submit_ingest_task(
                State(Arc::clone(&state)),
                Json(TaskIngestRequest {
                    source_id: None,
                    force: false,
                    embedding_profile_id: None,
                    vectors_only: false,
                }),
            ),
        )
        .await;
        release_tx.send(()).unwrap();
        lock_handle.await.unwrap();
        let Json(created) = submitted
            .expect("background all-source ingest submit should not wait for pipeline")
            .unwrap();

        assert!(!created.task_id.is_empty());
    }

    #[tokio::test]
    async fn public_background_ingest_batch_tags_children_and_restart_skip_cancels_them() {
        let test_dir = TestDir::new("ingest-batch-public-restart-skip");
        let first_path = test_dir.path().join("first.md");
        let second_path = test_dir.path().join("second.md");
        fs::write(&first_path, "first batch source").unwrap();
        fs::write(&second_path, "second batch source").unwrap();
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let first_id = pipeline.add_source(&first_path).unwrap();
        let second_id = pipeline.add_source(&second_path).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let first_hash = current_source_hash_for_test(&state, &first_id);
        let second_hash = current_source_hash_for_test(&state, &second_id);
        state.ingest_queue_active.store(true, Ordering::Release);

        let Json(created) = submit_ingest_task(
            State(Arc::clone(&state)),
            Json(TaskIngestRequest {
                source_id: None,
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap();
        state.ingest_queue_active.store(false, Ordering::Release);
        let parent_id = TaskId(created.task_id);
        assert!(ingest_batch_children_for_test(&state, &parent_id).is_empty());
        assert!(expand_next_unexpanded_ingest_batch(&state).await.unwrap());
        let children = ingest_batch_children_for_test(&state, &parent_id);

        assert_eq!(children.len(), 2);
        let child_source_ids = children
            .iter()
            .map(|task| task.request["source_id"].as_str().unwrap().to_string())
            .collect::<BTreeSet<_>>();
        assert_eq!(
            child_source_ids,
            BTreeSet::from([first_id.0.clone(), second_id.0.clone()])
        );
        let child_source_hashes = children
            .iter()
            .map(|task| {
                (
                    task.request["source_id"].as_str().unwrap().to_string(),
                    task.request["source_hash"].as_str().unwrap().to_string(),
                )
            })
            .collect::<BTreeMap<_, _>>();
        assert_eq!(
            child_source_hashes
                .get(first_id.0.as_str())
                .map(String::as_str),
            Some(first_hash.as_str())
        );
        assert_eq!(
            child_source_hashes
                .get(second_id.0.as_str())
                .map(String::as_str),
            Some(second_hash.as_str())
        );
        assert!(children.iter().all(|task| {
            task.status == TaskStatus::Queued
                && task.request["ingest_batch_id"] == parent_id.0
                && task.request["queue_claimable"] == true
        }));

        let Json(cancelled) =
            cancel_task_handler(State(Arc::clone(&state)), Path(parent_id.clone().0))
                .await
                .unwrap();
        assert_eq!(cancelled.task.status, TaskStatus::Cancelled);

        let claimed = claim_startable_ingest_task(&state).await.unwrap();

        assert!(claimed.is_none());
        let children = ingest_batch_children_for_test(&state, &parent_id);
        assert!(children
            .iter()
            .all(|task| task.status == TaskStatus::Cancelled));
    }

    #[tokio::test]
    async fn empty_background_ingest_batch_parent_records_terminalize_span() {
        let test_dir = TestDir::new("ingest-batch-empty-terminalize");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        state.ingest_queue_active.store(true, Ordering::Release);

        let Json(created) = submit_ingest_task(
            State(Arc::clone(&state)),
            Json(TaskIngestRequest {
                source_id: None,
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap();
        state.ingest_queue_active.store(false, Ordering::Release);
        let parent_id = TaskId(created.task_id);

        assert!(expand_next_unexpanded_ingest_batch(&state).await.unwrap());
        let parent_response = task_summary_response(&state, parent_id).await.unwrap();

        assert_eq!(parent_response.task.status, TaskStatus::Succeeded);
        assert!(has_task_terminalize_span(&parent_response.spans));
    }

    #[tokio::test]
    async fn public_background_ingest_batch_parent_succeeds_after_children_succeed() {
        let test_dir = TestDir::new("ingest-batch-public-parent-success");
        let first_path = test_dir.path().join("first.md");
        let second_path = test_dir.path().join("second.md");
        fs::write(&first_path, "first batch source").unwrap();
        fs::write(&second_path, "second batch source").unwrap();
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        pipeline.add_source(&first_path).unwrap();
        pipeline.add_source(&second_path).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        state.ingest_queue_active.store(true, Ordering::Release);

        let Json(created) = submit_ingest_task(
            State(Arc::clone(&state)),
            Json(TaskIngestRequest {
                source_id: None,
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap();
        state.ingest_queue_active.store(false, Ordering::Release);
        let parent_id = TaskId(created.task_id);
        assert!(expand_next_unexpanded_ingest_batch(&state).await.unwrap());
        let children = ingest_batch_children_for_test(&state, &parent_id);
        assert_eq!(children.len(), 2);

        finish_task_success(
            &state,
            &children[0].id,
            ingest_result_metadata(1, &EmbeddingCacheStats::default()),
        )
        .await
        .unwrap();
        let parent = task_summary_response(&state, parent_id.clone())
            .await
            .unwrap()
            .task;
        assert_eq!(parent.status, TaskStatus::Queued);

        finish_task_success(
            &state,
            &children[1].id,
            ingest_result_metadata(1, &EmbeddingCacheStats::default()),
        )
        .await
        .unwrap();

        let parent_response = task_summary_response(&state, parent_id).await.unwrap();
        let parent = parent_response.task;
        assert_eq!(parent.status, TaskStatus::Succeeded);
        assert!(has_task_terminalize_span(&parent_response.spans));
        assert_eq!(parent.result.unwrap()["ingested"], 2);
    }

    #[tokio::test]
    async fn background_ingest_batch_indexes_json_source_and_succeeds_parent() {
        use verbatim_core::collection::{CollectionMemberCandidate, CollectionSyncReport};

        let test_dir = TestDir::new("ingest-batch-json-source");
        let source_path = test_dir.path().join("article.json");
        fs::write(
            &source_path,
            serde_json::json!({
                "title": "Durable JSON article",
                "body": "JSON collection members should not fail all-source ingest"
            })
            .to_string(),
        )
        .unwrap();
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.embedding.enabled = false;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        pipeline.store().create_collection("articles", &[]).unwrap();
        pipeline
            .store()
            .replace_collection_members(
                "articles",
                &[CollectionMemberCandidate {
                    source_id: source_id.clone(),
                    logical_path: "article.json".into(),
                    source_path: fs::canonicalize(&source_path).unwrap(),
                }],
                CollectionSyncReport {
                    member_count: 0,
                    added: 0,
                    removed: 0,
                    unchanged: 0,
                    scanned_roots: 1,
                    max_depth: 32,
                    skipped: Vec::new(),
                },
            )
            .unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        state.ingest_queue_active.store(true, Ordering::Release);

        let Json(created) = submit_ingest_task(
            State(Arc::clone(&state)),
            Json(TaskIngestRequest {
                source_id: None,
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap();
        state.ingest_queue_active.store(false, Ordering::Release);
        let parent_id = TaskId(created.task_id);
        assert!(expand_next_unexpanded_ingest_batch(&state).await.unwrap());
        let children = ingest_batch_children_for_test(&state, &parent_id);
        assert_eq!(children.len(), 1);
        assert_eq!(
            children[0].request["source_id"].as_str(),
            Some(source_id.0.as_str())
        );

        schedule_ingest_queue(Arc::clone(&state));
        wait_for_task_status(&state, &parent_id, TaskStatus::Succeeded).await;

        let parent = task_summary_response(&state, parent_id).await.unwrap().task;
        assert_eq!(parent.result.unwrap()["failed_children"], 0);
        let pipeline = state.pipeline.lock().unwrap();
        let pipeline = pipeline.as_ref().unwrap();
        let source = pipeline.store().get_source(&source_id).unwrap().unwrap();
        assert_eq!(source.status, SourceStatus::Indexed);
        assert_eq!(source.parser_used.as_deref(), Some("json"));
        assert!(pipeline.check_stale().unwrap().is_empty());
    }

    #[tokio::test]
    async fn background_ingest_batch_prunes_missing_source_removed_by_collection_sync() {
        use verbatim_core::collection::{CollectionMemberCandidate, CollectionSyncReport};

        let test_dir = TestDir::new("ingest-batch-missing-source");
        let missing_path = test_dir.path().join("removed.md");
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.embedding.enabled = false;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = SourceId("removed-md".into());
        pipeline
            .store()
            .add_source(&Source {
                id: source_id.clone(),
                path: missing_path.clone(),
                hash: "old-hash".into(),
                status: SourceStatus::Stale,
                parser_used: Some("markdown".into()),
                last_ingested_at: None,
            })
            .unwrap();
        pipeline.store().create_collection("articles", &[]).unwrap();
        let report = || CollectionSyncReport {
            member_count: 0,
            added: 0,
            removed: 0,
            unchanged: 0,
            scanned_roots: 1,
            max_depth: 32,
            skipped: Vec::new(),
        };
        pipeline
            .store()
            .replace_collection_members(
                "articles",
                &[CollectionMemberCandidate {
                    source_id: source_id.clone(),
                    logical_path: "removed.md".into(),
                    source_path: missing_path,
                }],
                report(),
            )
            .unwrap();
        pipeline
            .store()
            .replace_collection_members("articles", &[], report())
            .unwrap();
        assert!(pipeline
            .store()
            .list_collection_members("articles")
            .unwrap()
            .is_empty());
        let state = test_state(config, test_dir.path(), pipeline);
        state.ingest_queue_active.store(true, Ordering::Release);

        let Json(created) = submit_ingest_task(
            State(Arc::clone(&state)),
            Json(TaskIngestRequest {
                source_id: None,
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap();
        state.ingest_queue_active.store(false, Ordering::Release);
        let parent_id = TaskId(created.task_id);

        assert!(expand_next_unexpanded_ingest_batch(&state).await.unwrap());

        let parent = task_summary_response(&state, parent_id).await.unwrap().task;
        assert_eq!(parent.status, TaskStatus::Succeeded);
        let result = parent.result.unwrap();
        assert_eq!(result["ingested"], 0);
        assert_eq!(result["skipped_missing_sources"], 1);
        assert_eq!(result["failed_children"], 0);
        let children = ingest_batch_children_for_test(
            &state,
            &TaskId(result["ingest_batch_id"].as_str().unwrap().into()),
        );
        assert_eq!(children.len(), 1);
        assert_eq!(children[0].status, TaskStatus::Succeeded);
        assert_eq!(
            children[0]
                .result
                .as_ref()
                .unwrap()
                .get("skipped_missing_sources"),
            Some(&serde_json::json!(1))
        );
        let pipeline = state.pipeline.lock().unwrap();
        let pipeline = pipeline.as_ref().unwrap();
        assert!(pipeline.store().get_source(&source_id).unwrap().is_none());
        assert!(pipeline.check_stale().unwrap().is_empty());
    }

    #[tokio::test]
    async fn background_vectors_only_ingest_keeps_missing_source_with_stored_chunks() {
        let model_server = MockModelServer::start(3).await;
        let test_dir = TestDir::new("ingest-batch-vectors-only-missing-source");
        let source_path = test_dir.path().join("removed.txt");
        fs::write(
            &source_path,
            "vector-only rebuild should use stored chunks even when source path is gone. "
                .repeat(80),
        )
        .unwrap();
        let config = retrieve_test_config(&model_server.base_url);
        let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        pipeline.ingest_source(&source_id).await.unwrap();
        let child_count = pipeline
            .store()
            .list_child_chunks()
            .unwrap()
            .into_iter()
            .filter(|chunk| chunk.source_id == source_id)
            .count();
        assert!(child_count > 0);
        fs::remove_file(&source_path).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        state.ingest_queue_active.store(true, Ordering::Release);

        let Json(created) = submit_ingest_task(
            State(Arc::clone(&state)),
            Json(TaskIngestRequest {
                source_id: None,
                force: false,
                embedding_profile_id: Some("alt-batch".into()),
                vectors_only: true,
            }),
        )
        .await
        .unwrap();
        state.ingest_queue_active.store(false, Ordering::Release);
        let parent_id = TaskId(created.task_id);

        assert!(expand_next_unexpanded_ingest_batch(&state).await.unwrap());
        let children = ingest_batch_children_for_test(&state, &parent_id);
        assert_eq!(children.len(), 1);
        assert_eq!(
            children[0].request["source_id"].as_str(),
            Some(source_id.0.as_str())
        );
        assert!(children[0].request.get("source_hash").is_none());

        schedule_ingest_queue(Arc::clone(&state));
        wait_for_task_status(&state, &parent_id, TaskStatus::Succeeded).await;

        let parent = task_summary_response(&state, parent_id).await.unwrap().task;
        let result = parent.result.unwrap();
        assert_eq!(result["ingested"], 1);
        assert_eq!(result["skipped_missing_sources"], 0);
        assert_eq!(result["failed_children"], 0);
        let pipeline = state.pipeline.lock().unwrap();
        let pipeline = pipeline.as_ref().unwrap();
        assert!(pipeline.store().get_source(&source_id).unwrap().is_some());
        assert_eq!(
            pipeline
                .store()
                .count_vector_documents_for_profile(
                    &EmbeddingProfileId::new("alt-batch").unwrap(),
                    Some(&source_id),
                )
                .unwrap(),
            child_count
        );
    }

    #[tokio::test]
    async fn foreground_single_source_ingest_request_includes_source_hash() {
        let test_dir = TestDir::new("foreground-ingest-source-hash");
        let source_path = test_dir.path().join("doc.txt");
        fs::write(&source_path, "foreground source hash").unwrap();
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.embedding.enabled = false;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let source_hash = current_source_hash_for_test(&state, &source_id);

        let Json(response) = ingest_one(
            State(Arc::clone(&state)),
            Path(source_id.0.clone()),
            Query(IngestQuery {
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap();

        assert_eq!(response.ingested, 1);
        let tasks = {
            let store = state.task_store.lock().unwrap();
            store
                .list_tasks_page(TaskListFilter::All, 10)
                .unwrap()
                .tasks
        };
        assert_eq!(tasks.len(), 1);
        assert_eq!(
            tasks[0]
                .request
                .get("source_hash")
                .and_then(serde_json::Value::as_str),
            Some(source_hash.as_str())
        );
    }

    #[tokio::test]
    async fn foreground_single_source_ingest_returns_busy_when_ingest_running() {
        let test_dir = TestDir::new("foreground-ingest-busy");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let running_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, false),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(&state, &running_id)
            .await
            .unwrap();

        let (status, Json(error)) = ingest_one(
            State(Arc::clone(&state)),
            Path("src-priority".into()),
            Query(IngestQuery {
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(error.error.contains("ingest queue busy"));
        let active = {
            let store = state.task_store.lock().unwrap();
            store.active_tasks(TaskKind::Ingest).unwrap()
        };
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, running_id);
    }

    #[tokio::test]
    async fn cancelling_running_batch_child_keeps_foreground_ingest_busy_until_worker_exits() {
        let test_dir = TestDir::new("batch-child-cancel-worker-busy");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let parent_id = TaskId::new();
        create_persisted_task_with_id(
            &state,
            parent_id.clone(),
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim_and_batch(
                None,
                false,
                None,
                false,
                false,
                Some(&parent_id.0),
            ),
        )
        .await
        .unwrap();
        let child_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim_and_batch(
                Some("src-child"),
                false,
                None,
                false,
                true,
                Some(&parent_id.0),
            ),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(&state, &child_id).await.unwrap();
        state.ingest_worker_active.store(true, Ordering::Release);

        let Json(cancelled) =
            cancel_task_handler(State(Arc::clone(&state)), Path(parent_id.clone().0))
                .await
                .unwrap();
        assert_eq!(cancelled.task.status, TaskStatus::Cancelled);
        assert_eq!(running_ingest_count(&state).await, 0);

        let (status, Json(error)) = ingest_one(
            State(Arc::clone(&state)),
            Path("src-priority".into()),
            Query(IngestQuery {
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(error.error.contains("ingest queue busy"));
        state.ingest_worker_active.store(false, Ordering::Release);
    }

    #[tokio::test]
    async fn cancelling_running_ingest_keeps_foreground_ingest_busy_until_worker_exits() {
        let test_dir = TestDir::new("running-ingest-cancel-worker-busy");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let running_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, false),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(&state, &running_id)
            .await
            .unwrap();
        state.ingest_worker_active.store(true, Ordering::Release);

        let Json(cancelled) =
            cancel_task_handler(State(Arc::clone(&state)), Path(running_id.clone().0))
                .await
                .unwrap();
        assert_eq!(cancelled.task.status, TaskStatus::Cancelled);
        assert_eq!(running_ingest_count(&state).await, 0);

        let (status, Json(error)) = ingest_one(
            State(Arc::clone(&state)),
            Path("src-priority".into()),
            Query(IngestQuery {
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
            }),
        )
        .await
        .unwrap_err();

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(error.error.contains("ingest queue busy"));
        state.ingest_worker_active.store(false, Ordering::Release);
    }

    #[tokio::test]
    async fn queued_background_ingest_drains_after_running_ingest_finishes() {
        let test_dir = TestDir::new("ingest-queue-drain");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let (running_id, queued_id) = blocked_ingest_pair(&state).await;
        assert_queued_ingest_waits_for_running(&state, &queued_id).await;

        finish_task_success(
            &state,
            &running_id,
            ingest_result_metadata(0, &EmbeddingCacheStats::default()),
        )
        .await
        .unwrap();

        wait_for_task_status(&state, &queued_id, TaskStatus::Succeeded).await;
        let events = task_events_response(&state, queued_id, None, Some(10))
            .await
            .unwrap()
            .events;
        assert!(events.iter().any(|event| event.event_type == "started"));
        assert!(events.iter().any(|event| event.event_type == "succeeded"));
    }

    #[tokio::test]
    async fn queued_background_ingest_drains_after_running_ingest_fails() {
        let test_dir = TestDir::new("ingest-queue-drain-after-failure");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let (running_id, queued_id) = blocked_ingest_pair(&state).await;
        assert_queued_ingest_waits_for_running(&state, &queued_id).await;

        finish_task_failed(&state, &running_id, "foreground ingest failed")
            .await
            .unwrap();

        wait_for_task_status(&state, &queued_id, TaskStatus::Succeeded).await;
        let events = task_events_response(&state, queued_id, None, Some(10))
            .await
            .unwrap()
            .events;
        assert!(events.iter().any(|event| event.event_type == "started"));
        assert!(events.iter().any(|event| event.event_type == "succeeded"));
    }

    #[tokio::test]
    async fn failed_background_ingest_task_can_resume_by_same_task_id() {
        let test_dir = TestDir::new("resume-background-ingest");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let running_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, false),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(&state, &running_id)
            .await
            .unwrap();
        let failed_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(
                Some("src-1"),
                false,
                Some("profile-a"),
                true,
                true,
            ),
        )
        .await
        .unwrap();
        finish_task_failed(&state, &failed_id, "embedding provider failed")
            .await
            .unwrap();

        let failed = task_summary_response(&state, failed_id.clone())
            .await
            .unwrap()
            .task;
        assert_eq!(failed.status, TaskStatus::Failed);
        let resume_command = failed
            .result
            .as_ref()
            .and_then(|result| result.get("resume_command"))
            .and_then(serde_json::Value::as_str)
            .map(str::to_string);
        assert_eq!(
            resume_command,
            Some(format!("verbatim task resume {}", failed_id.0))
        );

        let Json(resumed) =
            resume_task_handler(State(Arc::clone(&state)), Path(failed_id.clone().0))
                .await
                .unwrap();

        assert_eq!(resumed.task.id, failed_id);
        assert_eq!(resumed.task.status, TaskStatus::Queued);
        assert_eq!(resumed.task.request["source_id"], TASK_TELEMETRY_REDACTED);
        assert!(resumed.task.result.is_none());
        assert!(resumed.task.error.is_none());
        let events = task_events_response(&state, failed_id.clone(), None, Some(10))
            .await
            .unwrap()
            .events;
        let failed_event = events
            .iter()
            .find(|event| event.event_type == "failed")
            .expect("failed event exists");
        assert_eq!(failed_event.payload["resumability"]["resumable"], true);
        assert_eq!(failed_event.payload["resumability"]["operation"], "ingest");
        let resumed_event = events
            .iter()
            .find(|event| event.event_type == "resumed")
            .expect("resumed event exists");
        assert_eq!(resumed_event.payload["resume_command"], {
            serde_json::Value::String(format!("verbatim task resume {}", failed_id.0))
        });

        let running = task_summary_response(&state, running_id)
            .await
            .unwrap()
            .task;
        assert_eq!(running.status, TaskStatus::Running);
    }

    #[tokio::test]
    async fn failed_ask_task_resume_returns_conflict_without_mutation() {
        let test_dir = TestDir::new("resume-ask-conflict");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let task_id = create_persisted_task(
            &state,
            TaskKind::Ask,
            ask_request_metadata("What is cited?", None, None, false, false),
        )
        .await
        .unwrap();
        finish_task_failed(&state, &task_id, "chat provider failed")
            .await
            .unwrap();

        let (status, Json(error)) =
            resume_task_handler(State(Arc::clone(&state)), Path(task_id.clone().0))
                .await
                .unwrap_err();

        assert_eq!(status, StatusCode::CONFLICT);
        assert!(error.error.contains("task is not resumable"));
        let task = task_summary_response(&state, task_id.clone())
            .await
            .unwrap()
            .task;
        assert_eq!(task.status, TaskStatus::Failed);
        assert!(task.result.is_none());
        let events = task_events_response(&state, task_id, None, Some(10))
            .await
            .unwrap()
            .events;
        assert!(!events.iter().any(|event| event.event_type == "resumed"));
    }

    #[tokio::test]
    async fn background_queue_does_not_claim_foreground_ingest_task() {
        let test_dir = TestDir::new("foreground-ingest-not-claimed");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let foreground_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, false),
        )
        .await
        .unwrap();

        let (sender, receipt) = tokio::sync::oneshot::channel();
        *state.ingest_queue_drain_receipt.lock().unwrap() = Some(sender);
        schedule_ingest_queue(Arc::clone(&state));
        wait_for_ingest_queue_drain(receipt).await;

        let foreground = task_summary_response(&state, foreground_id.clone())
            .await
            .unwrap()
            .task;
        assert_eq!(foreground.status, TaskStatus::Queued);
        let response = execute_ingest_task(
            Arc::clone(&state),
            foreground_id.clone(),
            IndexingTaskControls {
                source_id: None,
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
                ingest_batch_id: None,
            },
        )
        .await
        .unwrap();

        assert_eq!(response.ingested, 0);
        wait_for_task_status(&state, &foreground_id, TaskStatus::Succeeded).await;
    }

    #[tokio::test]
    async fn background_queue_skips_unclaimable_foreground_head() {
        let test_dir = TestDir::new("foreground-head-does-not-starve-background");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let foreground_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, false),
        )
        .await
        .unwrap();
        let background_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, true),
        )
        .await
        .unwrap();

        schedule_ingest_queue(Arc::clone(&state));

        wait_for_task_status(&state, &background_id, TaskStatus::Succeeded).await;
        let foreground = task_summary_response(&state, foreground_id)
            .await
            .unwrap()
            .task;
        assert_eq!(foreground.status, TaskStatus::Queued);
        assert_eq!(running_ingest_count(&state).await, 0);
    }

    #[tokio::test]
    async fn foreground_start_waits_when_background_claimed_behind_unclaimable_head() {
        let test_dir = TestDir::new("foreground-start-waits-after-background-claim");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let foreground_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, false),
        )
        .await
        .unwrap();
        let background_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, true),
        )
        .await
        .unwrap();

        let claimed = claim_startable_ingest_task(&state).await.unwrap().unwrap();
        assert_eq!(claimed.id, background_id);
        let foreground_start = try_mark_ingest_task_started(&state, &foreground_id)
            .await
            .unwrap();

        assert_eq!(foreground_start, TaskStartOutcome::BlockedByRunningIngest);
        assert_eq!(running_ingest_count(&state).await, 1);
        assert_queued_ingest_waits_for_running(&state, &foreground_id).await;

        finish_task_success(
            &state,
            &background_id,
            ingest_result_metadata(0, &EmbeddingCacheStats::default()),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(&state, &foreground_id)
            .await
            .unwrap();

        assert_eq!(running_ingest_count(&state).await, 1);
        finish_task_success(
            &state,
            &foreground_id,
            ingest_result_metadata(0, &EmbeddingCacheStats::default()),
        )
        .await
        .unwrap();
    }

    #[tokio::test]
    async fn background_batch_claims_sibling_sources_up_to_embedding_window() {
        let test_dir = TestDir::new("background-batch-claim-window");
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.embedding.batch_size = 2;
        config.embedding.endpoint_runtime.max_concurrent_requests = 2;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let parent_id = TaskId::new();
        create_persisted_task_with_id(
            &state,
            parent_id.clone(),
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim_and_batch(
                None,
                false,
                None,
                false,
                false,
                Some(&parent_id.0),
            ),
        )
        .await
        .unwrap();
        let mut child_ids = Vec::new();
        for index in 0..5 {
            let child_id = create_persisted_task(
                &state,
                TaskKind::Ingest,
                ingest_task_request_metadata_with_queue_claim_and_batch(
                    Some(&format!("src-{index}")),
                    false,
                    None,
                    false,
                    true,
                    Some(&parent_id.0),
                ),
            )
            .await
            .unwrap();
            child_ids.push(child_id);
        }

        let claimed = claim_startable_ingest_work(&state).await.unwrap().unwrap();

        let ClaimedIngestWork::SourceBatch(tasks) = claimed else {
            panic!("expected source batch claim");
        };
        assert_eq!(tasks.len(), 4);
        let claimed_ids = tasks.iter().map(|task| task.id.clone()).collect::<Vec<_>>();
        assert!(claimed_ids
            .iter()
            .all(|task_id| child_ids.contains(task_id)));
        assert_eq!(running_ingest_count(&state).await, 4);
        let mut queued_children = 0;
        for child_id in child_ids {
            let child = task_summary_response(&state, child_id).await.unwrap().task;
            if child.status == TaskStatus::Queued {
                queued_children += 1;
            }
        }
        assert_eq!(queued_children, 1);
    }

    #[tokio::test]
    async fn source_batch_finalization_fails_started_tasks_missing_core_outcomes() {
        let test_dir = TestDir::new("source-batch-missing-outcome");
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.embedding.batch_size = 2;
        config.embedding.endpoint_runtime.max_concurrent_requests = 1;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let first_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim_and_batch(
                Some("src-batch-first"),
                false,
                None,
                false,
                true,
                Some("parent-batch"),
            ),
        )
        .await
        .unwrap();
        let second_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim_and_batch(
                Some("src-batch-second"),
                false,
                None,
                false,
                true,
                Some("parent-batch"),
            ),
        )
        .await
        .unwrap();

        let claimed = claim_startable_ingest_work(&state).await.unwrap().unwrap();
        let ClaimedIngestWork::SourceBatch(tasks) = claimed else {
            panic!("expected source batch claim");
        };
        assert_eq!(tasks.len(), 2);
        // The production batch executor holds this lease while finalizing outcomes.
        // Without it, per-child terminalization can wake abandoned-child recovery
        // while this fixture is still completing the same started batch.
        let _worker = acquire_ingest_worker(&state).unwrap();

        finish_started_ingest_source_batch_outcomes(
            &state,
            tasks,
            vec![SourceIngestOutcome {
                source_id: SourceId("src-batch-first".into()),
                task_id: first_id.clone(),
                result: Ok(EmbeddingCacheStats::default()),
            }],
        )
        .await
        .unwrap();

        let first_response = task_summary_response(&state, first_id.clone())
            .await
            .unwrap();
        let second_response = task_summary_response(&state, second_id.clone())
            .await
            .unwrap();
        let first = first_response.task;
        let second = second_response.task;
        assert_eq!(first.status, TaskStatus::Succeeded);
        assert_eq!(second.status, TaskStatus::Failed);
        assert!(has_task_terminalize_span(&first_response.spans));
        assert!(has_task_terminalize_span(&second_response.spans));
        assert!(second
            .error
            .as_deref()
            .unwrap_or_default()
            .contains("missing source batch outcome"));
        assert_eq!(running_ingest_count(&state).await, 0);
    }

    #[tokio::test]
    async fn source_batch_streamed_outcomes_are_idempotent_with_final_pass() {
        let test_dir = TestDir::new("source-batch-streamed-outcomes-idempotent");
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.embedding.batch_size = 2;
        config.embedding.endpoint_runtime.max_concurrent_requests = 1;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let parent_id = TaskId::new();
        create_persisted_task_with_id(
            &state,
            parent_id.clone(),
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim_and_batch(
                None,
                false,
                None,
                false,
                false,
                Some(&parent_id.0),
            ),
        )
        .await
        .unwrap();
        let first_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim_and_batch(
                Some("src-batch-stream-first"),
                false,
                None,
                false,
                true,
                Some(&parent_id.0),
            ),
        )
        .await
        .unwrap();
        let second_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim_and_batch(
                Some("src-batch-stream-second"),
                false,
                None,
                false,
                true,
                Some(&parent_id.0),
            ),
        )
        .await
        .unwrap();

        let claimed = claim_startable_ingest_work(&state).await.unwrap().unwrap();
        let ClaimedIngestWork::SourceBatch(tasks) = claimed else {
            panic!("expected source batch claim");
        };
        assert_eq!(tasks.len(), 2);
        let _worker = acquire_ingest_worker(&state).unwrap();
        let first_outcome = SourceIngestOutcome {
            source_id: SourceId("src-batch-stream-first".into()),
            task_id: first_id.clone(),
            result: Ok(EmbeddingCacheStats::default()),
        };
        let second_outcome = SourceIngestOutcome {
            source_id: SourceId("src-batch-stream-second".into()),
            task_id: second_id.clone(),
            result: Ok(EmbeddingCacheStats::default()),
        };

        finish_source_batch_task_outcome(&state, first_outcome.clone())
            .await
            .unwrap();

        let first = task_summary_response(&state, first_id.clone())
            .await
            .unwrap()
            .task;
        let second = task_summary_response(&state, second_id.clone())
            .await
            .unwrap()
            .task;
        assert_eq!(first.status, TaskStatus::Succeeded);
        assert_eq!(second.status, TaskStatus::Running);
        assert_eq!(running_ingest_count(&state).await, 1);

        finish_started_ingest_source_batch_outcomes(
            &state,
            tasks,
            vec![first_outcome, second_outcome],
        )
        .await
        .unwrap();

        let first = task_summary_response(&state, first_id.clone())
            .await
            .unwrap()
            .task;
        let second = task_summary_response(&state, second_id).await.unwrap().task;
        assert_eq!(first.status, TaskStatus::Succeeded);
        assert_eq!(second.status, TaskStatus::Succeeded);
        assert_eq!(running_ingest_count(&state).await, 0);
        let succeeded_events = {
            let store = state.task_store.lock().unwrap();
            store
                .list_task_events(&first_id, None, 10)
                .unwrap()
                .into_iter()
                .filter(|event| event.event_type == "succeeded")
                .count()
        };
        assert_eq!(succeeded_events, 1);
    }

    #[tokio::test]
    async fn ingest_queue_recovers_abandoned_running_source_batch_children() {
        let test_dir = TestDir::new("source-batch-abandoned-running");
        let mut config = retrieve_test_config("http://127.0.0.1:9/v1");
        config.embedding.batch_size = 2;
        config.embedding.endpoint_runtime.max_concurrent_requests = 1;
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let parent_id = TaskId::new();
        create_persisted_task_with_id(
            &state,
            parent_id.clone(),
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim_and_batch(
                None,
                false,
                None,
                false,
                false,
                Some(&parent_id.0),
            ),
        )
        .await
        .unwrap();
        let mut child_ids = Vec::new();
        for source_id in ["stalled-source-one", "stalled-source-two"] {
            let child_id = create_persisted_task(
                &state,
                TaskKind::Ingest,
                ingest_task_request_metadata_with_queue_claim_and_batch(
                    Some(source_id),
                    false,
                    None,
                    false,
                    true,
                    Some(&parent_id.0),
                ),
            )
            .await
            .unwrap();
            child_ids.push(child_id);
        }
        let claimed = claim_startable_ingest_work(&state).await.unwrap().unwrap();
        let ClaimedIngestWork::SourceBatch(tasks) = claimed else {
            panic!("expected source batch claim");
        };
        assert_eq!(tasks.len(), 2);
        {
            let store = state.task_store.lock().unwrap();
            let now = unix_timestamp_string();
            for task in &tasks {
                store
                    .update_task_progress(
                        &task.id,
                        TaskProgressSnapshot::phase(IngestTaskStage::EmbeddingPostprocess.as_str())
                            .with_recent_status("embedding complete"),
                    )
                    .unwrap();
                store
                    .insert_task_span(
                        &task.id,
                        IngestTaskStage::SqliteWrite.as_str(),
                        &now,
                        1280,
                        &serde_json::json!({ "operation": "replace_source_contents" }),
                    )
                    .unwrap();
                store
                    .insert_task_span(
                        &task.id,
                        IngestTaskStage::VectorIndex.as_str(),
                        &now,
                        2058,
                        &serde_json::json!({}),
                    )
                    .unwrap();
            }
        }
        assert_eq!(running_ingest_count(&state).await, 2);
        let followup_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, true),
        )
        .await
        .unwrap();
        let (locked_tx, locked_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel();
        let state_for_lock = Arc::clone(&state);
        let lock_handle = tokio::task::spawn_blocking(move || {
            let _pipeline = state_for_lock.pipeline.lock().unwrap();
            let _ = locked_tx.send(());
            release_rx.recv().unwrap();
        });
        locked_rx.await.unwrap();
        schedule_ingest_queue(Arc::clone(&state));
        wait_for_task_status(&state, &parent_id, TaskStatus::Failed).await;
        wait_for_task_status(&state, &followup_id, TaskStatus::Running).await;
        let running_after_parent_failed = running_ingest_count(&state).await;
        release_tx.send(()).unwrap();
        lock_handle.await.unwrap();
        wait_for_task_status(&state, &followup_id, TaskStatus::Succeeded).await;
        assert_eq!(running_after_parent_failed, 1);
        assert_eq!(running_ingest_count(&state).await, 0);
        for child_id in child_ids {
            let child_response = task_summary_response(&state, child_id).await.unwrap();
            let child = child_response.task;
            assert_eq!(child.status, TaskStatus::Failed);
            assert!(has_task_terminalize_span(&child_response.spans));
            assert!(child
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("abandoned running source-batch ingest task"));
            assert_eq!(child.result.as_ref().unwrap()["resumable"], true);
        }
        let parent_response = task_summary_response(&state, parent_id).await.unwrap();
        assert!(has_task_terminalize_span(&parent_response.spans));
    }

    #[tokio::test]
    async fn background_queue_claim_waits_when_foreground_starts_after_candidate_selection() {
        let test_dir = TestDir::new("foreground-starts-before-background-claim");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let background_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, true),
        )
        .await
        .unwrap();
        {
            let store = state.task_store.lock().unwrap();
            let candidate = next_queue_claimable_ingest_task(&store).unwrap().unwrap();
            assert_eq!(candidate.id, background_id);
        }
        let foreground_id = create_persisted_task(
            &state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, false),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(&state, &foreground_id)
            .await
            .unwrap();

        let claimed = claim_startable_ingest_task(&state).await.unwrap();
        assert!(claimed.is_none());
        assert_queued_ingest_waits_for_running(&state, &background_id).await;

        finish_task_success(
            &state,
            &foreground_id,
            ingest_result_metadata(0, &EmbeddingCacheStats::default()),
        )
        .await
        .unwrap();

        wait_for_task_status(&state, &background_id, TaskStatus::Succeeded).await;
    }

    fn test_retrieval_result(
        rank: usize,
        chunk_id: &str,
        evidence_id: &str,
        kind: EvidenceKind,
    ) -> RetrievalResult {
        let source_id = SourceId("src".into());
        let chunk_id = ChunkId(chunk_id.into());
        let evidence = EvidenceUnit {
            id: EvidenceId(evidence_id.into()),
            source_id: source_id.clone(),
            kind,
            derived_from: None,
            locator: SourceLocator::Document {
                path_or_url: "doc.md".into(),
                line_start: 1,
                line_end: None,
            },
            text: format!("{evidence_id} text."),
            text_hash: format!("{evidence_id}-hash"),
            heading_path: Vec::new(),
            position: 0,
        };
        let chunk = Chunk {
            id: chunk_id.clone(),
            source_id: source_id.clone(),
            chunk_hash: format!("hash-{}", chunk_id.0),
            embedding_input_hash: None,
            text: evidence.text.clone(),
            context_text: None,
            token_count: 2,
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: Vec::new(),
            evidence_unit_ids: vec![evidence.id.clone()],
        };

        RetrievalResult {
            chunk_id: chunk_id.clone(),
            score: 1.0,
            chunk,
            evidence_units: vec![evidence],
            provenance: RetrievalProvenance::seed(rank, chunk_id, source_id),
        }
    }

    fn test_canonical_retrieval_result(
        rank: usize,
        chunk_id: &str,
        evidence: &[(&str, u32)],
    ) -> RetrievalResult {
        let source_id = SourceId("src".into());
        let chunk_id = ChunkId(chunk_id.into());
        let evidence_units = evidence
            .iter()
            .map(|(id, verse)| EvidenceUnit {
                id: EvidenceId((*id).into()),
                source_id: source_id.clone(),
                kind: EvidenceKind::Text,
                derived_from: None,
                locator: SourceLocator::Canonical {
                    locator: CanonicalLocator::single_unit(
                        "bible",
                        "CSB",
                        vec![
                            ReferenceComponent {
                                level: "book".into(),
                                value: "2 Timothy".into(),
                                ordinal: Some(55),
                            },
                            ReferenceComponent {
                                level: "chapter".into(),
                                value: "4".into(),
                                ordinal: Some(4),
                            },
                            ReferenceComponent {
                                level: "verse".into(),
                                value: verse.to_string(),
                                ordinal: Some(*verse),
                            },
                        ],
                        format!("2 Timothy 4:{verse}"),
                        format!("2timothy:4:{verse}"),
                    ),
                },
                text: format!("verse {verse} text."),
                text_hash: format!("{id}-hash"),
                heading_path: Vec::new(),
                position: *verse,
            })
            .collect::<Vec<_>>();
        let chunk = Chunk {
            id: chunk_id.clone(),
            source_id: source_id.clone(),
            chunk_hash: format!("hash-{}", chunk_id.0),
            embedding_input_hash: None,
            text: evidence_units
                .iter()
                .map(|unit| unit.text.as_str())
                .collect::<Vec<_>>()
                .join(" "),
            context_text: None,
            token_count: evidence_units.len() as u32 * 2,
            chunk_type: ChunkType::Child,
            parent_chunk_id: None,
            heading_path: Vec::new(),
            evidence_unit_ids: evidence_units.iter().map(|unit| unit.id.clone()).collect(),
        };

        RetrievalResult {
            chunk_id: chunk_id.clone(),
            score: 1.0,
            chunk,
            evidence_units,
            provenance: RetrievalProvenance::seed(rank, chunk_id, source_id),
        }
    }

    struct TestDir {
        path: PathBuf,
    }

    impl TestDir {
        fn new(name: &str) -> Self {
            let unique = format!(
                "verbatim-daemon-{name}-{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap()
                    .as_nanos()
            );
            let path = std::env::temp_dir().join(unique);
            fs::create_dir_all(&path).unwrap();
            Self { path }
        }

        fn path(&self) -> &FsPath {
            &self.path
        }
    }

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.path);
        }
    }

    pub(super) fn test_state(c: Config, dir: &FsPath, p: IngestPipeline) -> SharedState {
        test_state_with_config_path(c, dir, p, dir.join("config.toml"))
    }

    async fn ingest_source_for_test(state: &SharedState, source_id: &SourceId) {
        let task_id = create_persisted_task(
            state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(
                Some(&source_id.0),
                false,
                None,
                false,
                false,
            ),
        )
        .await
        .unwrap();
        let response = execute_ingest_task(
            Arc::clone(state),
            task_id,
            IndexingTaskControls {
                source_id: Some(source_id.0.clone()),
                force: false,
                embedding_profile_id: None,
                vectors_only: false,
                ingest_batch_id: None,
            },
        )
        .await
        .unwrap();
        assert_eq!(response.ingested, 1);
    }

    fn collection_member_ids_for_test(
        state: &SharedState,
        collection_name: &str,
    ) -> (SourceId, SourceId) {
        let pipeline = state.pipeline.lock().unwrap();
        let pipeline = pipeline.as_ref().unwrap();
        let members = pipeline
            .store()
            .list_collection_members(collection_name)
            .unwrap();
        assert!(!members.is_empty());
        let mut source_ids = members
            .into_iter()
            .map(|member| member.source_id)
            .collect::<Vec<_>>();
        if source_ids.len() == 1 {
            let only = source_ids.remove(0);
            return (only.clone(), only);
        }
        assert_eq!(source_ids.len(), 2);
        (source_ids.remove(0), source_ids.remove(0))
    }

    fn queued_source_ids_for_test(state: &SharedState) -> Vec<SourceId> {
        let store = state.task_store.lock().unwrap();
        store
            .queued_tasks(TaskKind::Ingest)
            .unwrap()
            .into_iter()
            .filter_map(|task| {
                task.request
                    .get("source_id")
                    .and_then(serde_json::Value::as_str)
                    .map(|id| SourceId(id.to_string()))
            })
            .collect()
    }

    fn active_ingest_intent_count_for_test(
        state: &SharedState,
        source_id: &SourceId,
        source_hash: &str,
    ) -> usize {
        let store = state.task_store.lock().unwrap();
        store
            .active_tasks(TaskKind::Ingest)
            .unwrap()
            .into_iter()
            .filter(|task| {
                let Ok(request) =
                    serde_json::from_value::<PersistedIngestRequest>(task.request.clone())
                else {
                    return false;
                };
                request.operation.as_deref().unwrap_or("ingest") == "ingest"
                    && request.source_id.as_deref() == Some(source_id.0.as_str())
                    && !request.vectors_only
                    && active_ingest_matches_source_hash(
                        Some(source_hash),
                        request.source_hash.as_deref(),
                    )
            })
            .count()
    }

    fn current_source_hash_for_test(state: &SharedState, source_id: &SourceId) -> String {
        let pipeline = state.pipeline.lock().unwrap();
        let pipeline = pipeline.as_ref().unwrap();
        pipeline
            .source_ingest_snapshot(source_id)
            .unwrap()
            .current_hash
            .unwrap()
    }

    fn ingest_request_with_source_hash_for_test(
        source_id: &SourceId,
        source_hash: &str,
    ) -> serde_json::Value {
        ingest_request_with_source_hash_and_vectors_only_for_test(source_id, source_hash, false)
    }

    fn ingest_request_with_source_hash_and_vectors_only_for_test(
        source_id: &SourceId,
        source_hash: &str,
        vectors_only: bool,
    ) -> serde_json::Value {
        let mut request = ingest_task_request_metadata_with_queue_claim(
            Some(&source_id.0),
            false,
            None,
            vectors_only,
            true,
        );
        if let serde_json::Value::Object(map) = &mut request {
            map.insert(
                "source_hash".into(),
                serde_json::Value::String(source_hash.to_string()),
            );
        }
        bounded_json(request)
    }

    fn config_watch_test_state(name: &str) -> (TestDir, SharedState) {
        let test_dir = TestDir::new(name);
        let config_path = test_dir.path().join("config.toml");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        fs::write(&config_path, config.show().unwrap()).unwrap();
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state_with_config_path(config, test_dir.path(), pipeline, config_path);
        (test_dir, state)
    }

    fn test_state_with_config_path(
        config: Config,
        data_dir: &FsPath,
        pipeline: IngestPipeline,
        config_path: PathBuf,
    ) -> SharedState {
        let index_status_cache = initial_index_status_cache(&pipeline);
        let memory_budget = pipeline.memory_budget();
        Arc::new(AppState {
            pipeline: std::sync::Mutex::new(Some(pipeline)),
            task_store: std::sync::Mutex::new(
                sqlite_durability_ops::open_task_store(&config, data_dir).unwrap(),
            ),
            index_status_cache: std::sync::RwLock::new(index_status_cache),
            readiness: std::sync::RwLock::new(ReadinessHealth::ready()),
            resources: daemon_resources(&config.daemon.resources),
            memory_budget,
            ingest_queue_active: AtomicBool::new(false),
            ingest_queue_drain_receipt: std::sync::Mutex::new(None),
            ingest_worker_active: AtomicBool::new(false),
            collection_watcher: CollectionWatcherRuntime::default(),
            idle_reclaim: Arc::new(IdleReclaimRuntime::new(now_unix_millis())),
            idle_exit: Arc::new(IdleExitRuntime::new(now_unix_millis())),
            #[cfg(test)]
            idle_reclaim_before_backend_hook: std::sync::Mutex::new(None),
            #[cfg(test)]
            idle_reclaim_before_backend_call_hook: std::sync::Mutex::new(None),
            runtime_config: std::sync::RwLock::new(RuntimeConfigState {
                reload: initial_reload_metadata(&config_path),
                config,
            }),
            config_path,
            data_dir: data_dir.to_path_buf(),
        })
    }

    async fn blocked_ingest_pair(state: &SharedState) -> (TaskId, TaskId) {
        let running_id = create_persisted_task(
            state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, false),
        )
        .await
        .unwrap();
        ensure_ingest_task_started(state, &running_id)
            .await
            .unwrap();
        let queued_id = create_persisted_task(
            state,
            TaskKind::Ingest,
            ingest_task_request_metadata_with_queue_claim(None, false, None, false, true),
        )
        .await
        .unwrap();
        (running_id, queued_id)
    }

    fn ingest_batch_children_for_test(
        state: &SharedState,
        parent_id: &TaskId,
    ) -> Vec<verbatim_core::task::TaskSummary> {
        let store = state.task_store.lock().unwrap();
        ingest_batch_children(&store, &parent_id.0).unwrap()
    }

    fn task_count_for_test(state: &SharedState) -> usize {
        let store = state.task_store.lock().unwrap();
        store.tasks_all().unwrap().len()
    }

    fn assert_starting_readiness_error(error: &ErrorResponse, startup_phase: &str) {
        assert_eq!(error.code.as_deref(), Some("retrieval_not_ready"));
        assert_eq!(error.readiness.as_deref(), Some("starting"));
        assert_eq!(error.retrieval_ready, Some(false));
        assert_eq!(error.startup_phase.as_deref(), Some(startup_phase));
        assert!(error.error.contains("verbatim daemon is starting"));
        assert!(error.error.contains("retrieval is not ready"));
    }

    async fn assert_queued_ingest_waits_for_running(state: &SharedState, queued_id: &TaskId) {
        let queued = task_summary_response(state, queued_id.clone())
            .await
            .unwrap()
            .task;
        assert_eq!(queued.status, TaskStatus::Queued);
        assert_eq!(queued.queue_position, Some(1));
        assert_eq!(
            queued.blocking_reason.as_deref(),
            Some("waiting for running ingest task to finish")
        );
    }

    async fn running_ingest_count(state: &SharedState) -> usize {
        let state = Arc::clone(state);
        tokio::task::spawn_blocking(move || {
            let store = state.task_store.lock().unwrap();
            store.count_running_tasks(TaskKind::Ingest).unwrap()
        })
        .await
        .unwrap()
    }

    async fn claim_startable_ingest_task(
        state: &SharedState,
    ) -> Result<Option<verbatim_core::task::TaskSummary>> {
        let Some(work) = claim_startable_ingest_work(state).await? else {
            return Ok(None);
        };
        Ok(match work {
            ClaimedIngestWork::Single(task) => Some(*task),
            ClaimedIngestWork::SourceBatch(tasks) => tasks.into_iter().next(),
        })
    }

    async fn wait_for_ingest_queue_drain(receipt: tokio::sync::oneshot::Receiver<()>) {
        tokio::time::timeout(Duration::from_secs(2), receipt)
            .await
            .expect("ingest queue drain receipt must complete")
            .expect("ingest queue drain receipt sender must remain available");
    }

    async fn wait_for_task_status(state: &SharedState, task_id: &TaskId, status: TaskStatus) {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let task = task_summary_response(state, task_id.clone())
                    .await
                    .unwrap()
                    .task;
                if task.status == status {
                    return;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
    }

    async fn wait_for_collection_watcher_status<F>(
        state: &SharedState,
        name: &str,
        is_ready: F,
    ) -> CollectionWatcherStatus
    where
        F: Fn(&CollectionWatcherStatus) -> bool,
    {
        tokio::time::timeout(Duration::from_secs(2), async {
            loop {
                let Json(response) =
                    collection_watcher_status(State(Arc::clone(state)), Path(name.to_string()))
                        .await
                        .unwrap();
                if is_ready(&response.watcher) {
                    return response.watcher;
                }
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap()
    }

    fn latest_task_id(state: &SharedState, kind: TaskKind) -> TaskId {
        let store = state.task_store.lock().unwrap();
        store
            .tasks(kind)
            .unwrap()
            .into_iter()
            .last()
            .expect("expected at least one task")
            .id
    }

    async fn http_get_health_for_test(addr: std::net::SocketAddr) -> HealthResponse {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream
            .write_all(b"GET /api/health HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n")
            .await
            .unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8(response).unwrap();
        assert!(
            response.starts_with("HTTP/1.1 200 OK"),
            "unexpected response: {response}"
        );
        let body = response
            .split_once("\r\n\r\n")
            .map(|(_, body)| body)
            .expect("HTTP response includes a body separator");
        serde_json::from_str(body).unwrap()
    }

    async fn http_post_ask_stream_for_test(addr: std::net::SocketAddr) -> (String, String, String) {
        use tokio::io::{AsyncReadExt, AsyncWriteExt};

        let body = r#"{"question":"question","context_only":true}"#;
        let request = format!(
            "POST /api/ask/stream HTTP/1.1\r\n\
             Host: 127.0.0.1\r\n\
             Content-Type: application/json\r\n\
             Content-Length: {}\r\n\
             Connection: close\r\n\r\n\
             {}",
            body.len(),
            body
        );
        let mut stream = tokio::net::TcpStream::connect(addr).await.unwrap();
        stream.write_all(request.as_bytes()).await.unwrap();
        let mut response = Vec::new();
        stream.read_to_end(&mut response).await.unwrap();
        let response = String::from_utf8(response).unwrap();
        let (head, body) = response
            .split_once("\r\n\r\n")
            .expect("HTTP response includes a body separator");
        let (status_line, headers) = head
            .split_once("\r\n")
            .expect("HTTP response includes a status line");

        (status_line.into(), headers.into(), body.into())
    }

    struct MockModelServer {
        base_url: String,
        model_requests: Arc<AtomicUsize>,
        model_blocked: Arc<AtomicBool>,
        model_release: Arc<tokio::sync::Notify>,
        embedding_requests: Arc<AtomicUsize>,
        embedding_blocked: Arc<AtomicBool>,
        embedding_release: Arc<tokio::sync::Notify>,
        chat_requests: Arc<AtomicUsize>,
        chat_payloads: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        handle: tokio::task::JoinHandle<()>,
    }

    impl MockModelServer {
        async fn start(dimension: usize) -> Self {
            Self::start_with_chat_response(dimension, None).await
        }

        async fn start_with_chat(dimension: usize, chat_response: impl Into<String>) -> Self {
            Self::start_with_chat_response(dimension, Some(chat_response.into())).await
        }

        async fn start_with_chat_responses(
            dimension: usize,
            chat_responses: impl IntoIterator<Item = impl Into<String>>,
        ) -> Self {
            Self::start_with_chat_response_queue(
                dimension,
                chat_responses
                    .into_iter()
                    .map(Into::into)
                    .collect::<VecDeque<_>>(),
            )
            .await
        }

        async fn start_with_chat_response(dimension: usize, chat_response: Option<String>) -> Self {
            Self::start_with_chat_response_queue(
                dimension,
                chat_response.into_iter().collect::<VecDeque<_>>(),
            )
            .await
        }

        async fn start_with_chat_response_queue(
            dimension: usize,
            chat_responses: VecDeque<String>,
        ) -> Self {
            let state = MockModelState {
                dimension,
                model_requests: Arc::new(AtomicUsize::new(0)),
                model_blocked: Arc::new(AtomicBool::new(false)),
                model_release: Arc::new(tokio::sync::Notify::new()),
                embedding_requests: Arc::new(AtomicUsize::new(0)),
                embedding_blocked: Arc::new(AtomicBool::new(false)),
                embedding_release: Arc::new(tokio::sync::Notify::new()),
                chat_requests: Arc::new(AtomicUsize::new(0)),
                chat_payloads: Arc::new(std::sync::Mutex::new(Vec::new())),
                chat_responses: Arc::new(std::sync::Mutex::new(chat_responses)),
                last_chat_response: Arc::new(std::sync::Mutex::new(None)),
            };
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let app = Router::new()
                .route("/models", get(mock_models))
                .route("/v1/models", get(mock_models))
                .route("/v1/embeddings", post(mock_embeddings))
                .route("/v1/chat/completions", post(mock_chat))
                .with_state(state.clone());
            let handle = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });

            Self {
                base_url: format!("http://{addr}/v1"),
                model_requests: state.model_requests,
                model_blocked: state.model_blocked,
                model_release: state.model_release,
                embedding_requests: state.embedding_requests,
                embedding_blocked: state.embedding_blocked,
                embedding_release: state.embedding_release,
                chat_requests: state.chat_requests,
                chat_payloads: state.chat_payloads,
                handle,
            }
        }

        fn model_requests(&self) -> usize {
            self.model_requests.load(Ordering::SeqCst)
        }

        fn block_models(&self) {
            self.model_blocked.store(true, Ordering::SeqCst);
        }

        fn release_models(&self) {
            self.model_blocked.store(false, Ordering::SeqCst);
            self.model_release.notify_waiters();
        }

        fn embedding_requests(&self) -> usize {
            self.embedding_requests.load(Ordering::SeqCst)
        }

        fn block_embeddings(&self) {
            self.embedding_blocked.store(true, Ordering::SeqCst);
        }

        fn release_embeddings(&self) {
            self.embedding_blocked.store(false, Ordering::SeqCst);
            self.embedding_release.notify_waiters();
        }

        fn chat_requests(&self) -> usize {
            self.chat_requests.load(Ordering::SeqCst)
        }

        fn chat_payloads(&self) -> Vec<serde_json::Value> {
            self.chat_payloads.lock().unwrap().clone()
        }
    }

    impl Drop for MockModelServer {
        fn drop(&mut self) {
            self.handle.abort();
        }
    }

    #[derive(Clone)]
    struct MockModelState {
        dimension: usize,
        model_requests: Arc<AtomicUsize>,
        model_blocked: Arc<AtomicBool>,
        model_release: Arc<tokio::sync::Notify>,
        embedding_requests: Arc<AtomicUsize>,
        embedding_blocked: Arc<AtomicBool>,
        embedding_release: Arc<tokio::sync::Notify>,
        chat_requests: Arc<AtomicUsize>,
        chat_payloads: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
        chat_responses: Arc<std::sync::Mutex<VecDeque<String>>>,
        last_chat_response: Arc<std::sync::Mutex<Option<String>>>,
    }

    async fn mock_models(State(state): State<MockModelState>) -> Json<serde_json::Value> {
        state.model_requests.fetch_add(1, Ordering::SeqCst);
        if state.model_blocked.load(Ordering::SeqCst) {
            state.model_release.notified().await;
        }
        Json(serde_json::json!({
            "data": [
                {
                    "id": "test-embedding",
                    "root": "test-embedding"
                }
            ]
        }))
    }

    async fn mock_embeddings(
        State(state): State<MockModelState>,
        Json(payload): Json<serde_json::Value>,
    ) -> Json<serde_json::Value> {
        state.embedding_requests.fetch_add(1, Ordering::SeqCst);
        if state.embedding_blocked.load(Ordering::SeqCst) {
            state.embedding_release.notified().await;
        }
        let input_count = payload
            .get("input")
            .and_then(serde_json::Value::as_array)
            .map_or(0, Vec::len);
        let embedding = {
            let mut values = vec![0.0; state.dimension];
            if let Some(first) = values.first_mut() {
                *first = 1.0;
            }
            values
        };
        let data = (0..input_count)
            .map(|_| serde_json::json!({ "embedding": embedding.clone() }))
            .collect::<Vec<_>>();
        Json(serde_json::json!({ "data": data }))
    }

    async fn mock_chat(
        State(state): State<MockModelState>,
        Json(payload): Json<serde_json::Value>,
    ) -> Response {
        state.chat_requests.fetch_add(1, Ordering::SeqCst);
        state.chat_payloads.lock().unwrap().push(payload.clone());
        let content = {
            let mut responses = state.chat_responses.lock().unwrap();
            responses.pop_front()
        }
        .or_else(|| state.last_chat_response.lock().unwrap().clone());
        if let Some(content) = content {
            *state.last_chat_response.lock().unwrap() = Some(content.clone());
            if payload["stream"].as_bool() == Some(true) {
                let content = serde_json::to_string(&content).unwrap();
                return (
                    StatusCode::OK,
                    [("content-type", "text/event-stream")],
                    format!(
                        "data: {{\"choices\":[{{\"delta\":{{\"content\":{content}}},\"finish_reason\":\"stop\"}}]}}\n\ndata: [DONE]\n\n"
                    ),
                )
                    .into_response();
            }
            return (
                StatusCode::OK,
                Json(serde_json::json!({
                    "choices": [
                        {
                            "message": { "content": content },
                            "finish_reason": "stop"
                        }
                    ]
                })),
            )
                .into_response();
        }
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "chat must not be called by retrieve" })),
        )
            .into_response()
    }

    pub(super) fn retrieve_test_config(model_base_url: &str) -> Config {
        serde_json::from_value(serde_json::json!({
            "embedding": {
                "base_url": model_base_url,
                "model": "test-embedding",
                "dimension": 3,
                "normalize": false,
                "query_instruction": "",
                "document_instruction": "",
                "batch_size": 16,
                "timeout_seconds": 1
            },
            "retrieval": {
                "dense_top_k": 4,
                "bm25_top_k": 4,
                "rrf_k": 60,
                "default_limit": 3,
                "default_page_size": 1
            },
            "context": {
                "enabled": false
            },
            "chat": {
                "enabled": false,
                "base_url": model_base_url,
                "model": "chat-must-not-be-called",
                "timeout_seconds": 1
            },
            "verifier": {
                "enabled": false
            }
        }))
        .unwrap()
    }

    fn idle_reclaim_test_config(
        model_base_url: &str,
        idle_timeout_seconds: u64,
        min_interval_seconds: u64,
        sqlite_shrink_memory: bool,
        malloc_trim: bool,
    ) -> Config {
        let mut config = retrieve_test_config(model_base_url);
        config.daemon.idle_reclaim.enabled = true;
        config.daemon.idle_reclaim.idle_timeout_seconds = idle_timeout_seconds;
        config.daemon.idle_reclaim.min_interval_seconds = min_interval_seconds;
        config.daemon.idle_reclaim.sqlite_shrink_memory = sqlite_shrink_memory;
        config.daemon.idle_reclaim.malloc_trim = malloc_trim;
        config
    }

    fn idle_exit_test_config(model_base_url: &str, timeout_seconds: u64) -> Config {
        let mut config = retrieve_test_config(model_base_url);
        config.daemon.idle_exit.enabled = true;
        config.daemon.idle_exit.timeout_seconds = timeout_seconds;
        config
    }

    fn force_idle_reclaim_timeout_elapsed(state: &SharedState, elapsed_seconds: u64) {
        state.idle_reclaim.last_activity_unix_ms.store(
            now_unix_millis().saturating_sub(elapsed_seconds.saturating_mul(1_000)),
            Ordering::Release,
        );
    }

    fn force_idle_exit_timeout_elapsed(state: &SharedState, elapsed_seconds: u64) {
        state.idle_exit.last_activity_unix_ms.store(
            now_unix_millis().saturating_sub(elapsed_seconds.saturating_mul(1_000)),
            Ordering::Release,
        );
    }

    fn idle_exit_resource_snapshot_for_test(active: usize, queued: usize) -> ResourceQueueSnapshot {
        idle_reclaim_resource_snapshot_for_test(active, queued)
    }

    fn idle_exit_resource_snapshots_for_test(_state: &SharedState) -> Vec<ResourceQueueSnapshot> {
        Vec::new()
    }

    fn assert_idle_reclaim_skipped_without_backends(
        result: &IdleReclaimCycleResult,
        skip_reason: &str,
    ) {
        assert_eq!(result.status, "skipped");
        assert_eq!(result.skip_reason.as_deref(), Some(skip_reason));
        assert!(!result.sqlite.attempted);
        assert!(!result.allocator.attempted);
    }

    fn run_idle_reclaim_initial_gate_for_test(
        state: &SharedState,
        resources: Vec<ResourceQueueSnapshot>,
    ) -> IdleReclaimCycleResult {
        let gate = idle_reclaim_gate_with_running(state, resources, false);
        let skip_reason = gate
            .health
            .skip_reason
            .expect("test resource snapshot should make idle reclaim skip");
        let result = skipped_idle_reclaim_result(skip_reason);
        state.idle_reclaim.record_result(result.clone());
        result
    }

    fn idle_reclaim_resource_snapshot_for_test(
        active: usize,
        queued: usize,
    ) -> ResourceQueueSnapshot {
        ResourceQueueSnapshot {
            name: "test_resource".into(),
            kind: "test".into(),
            capacity: 1,
            queue_capacity: 1,
            queued,
            active,
            completed: 0,
            errors: 0,
            queue_wait_ms_total: 0,
            service_ms_total: 0,
            last_queue_wait_ms: None,
            last_service_ms: None,
            throughput_per_minute: 0.0,
        }
    }

    fn set_idle_reclaim_before_backend_hook<F>(state: &SharedState, hook: F)
    where
        F: FnMut(&SharedState) + Send + 'static,
    {
        *state.idle_reclaim_before_backend_hook.lock().unwrap() = Some(Box::new(hook));
    }

    fn set_idle_reclaim_before_backend_call_hook<F>(state: &SharedState, hook: F)
    where
        F: FnMut(&SharedState) + Send + 'static,
    {
        *state.idle_reclaim_before_backend_call_hook.lock().unwrap() = Some(Box::new(hook));
    }

    async fn reloaded_embedding_endpoint_state(
        test_name: &str,
        chat_enabled_after_reload: bool,
    ) -> (TestDir, SharedState, SourceId, MockModelServer) {
        let old_model_server = MockModelServer::start(3).await;
        let new_model_server =
            MockModelServer::start_with_chat(3, "answer should not be generated [E1]").await;
        let test_dir = TestDir::new(test_name);
        let config_path = test_dir.path().join("config.toml");
        let source_path = test_dir.path().join("doc.md");
        fs::write(
            &source_path,
            "Alpha endpoint capability drift evidence for stale vector tests.",
        )
        .unwrap();
        let config = retrieve_test_config(&old_model_server.base_url);
        fs::write(&config_path, config.show().unwrap()).unwrap();

        let mut pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let source_id = pipeline.add_source(&source_path).unwrap();
        pipeline.ingest_source(&source_id).await.unwrap();
        assert!(
            pipeline
                .store()
                .count_vector_documents_for_profile(
                    &EmbeddingProfileId::default_profile(),
                    Some(&source_id),
                )
                .unwrap()
                > 0
        );

        let state =
            test_state_with_config_path(config.clone(), test_dir.path(), pipeline, config_path);
        let mut candidate = config.clone();
        candidate.embedding.base_url = new_model_server.base_url.clone();
        candidate.chat.enabled = chat_enabled_after_reload;
        candidate.chat.base_url = new_model_server.base_url.clone();
        candidate.chat.model = "test-chat".into();
        fs::write(&state.config_path, candidate.show().unwrap()).unwrap();
        reload_config_from_path(&state).await.unwrap();
        assert!(old_model_server.embedding_requests() > 0);
        assert!(default_profile_source_vector_count(&state, &source_id) > 0);

        (test_dir, state, source_id, new_model_server)
    }

    fn default_profile_source_vector_count(state: &SharedState, source_id: &SourceId) -> usize {
        state
            .pipeline
            .lock()
            .unwrap()
            .as_ref()
            .unwrap()
            .store()
            .count_vector_documents_for_profile(
                &EmbeddingProfileId::default_profile(),
                Some(source_id),
            )
            .unwrap()
    }

    fn add_single_source_collection_for_test(
        state: &SharedState,
        collection_name: &str,
        source_id: &SourceId,
    ) {
        use verbatim_core::collection::{CollectionMemberCandidate, CollectionSyncReport};

        let pipeline = state.pipeline.lock().unwrap();
        let pipeline = pipeline.as_ref().unwrap();
        let Some(source) = pipeline.store().get_source(source_id).unwrap() else {
            panic!("test source not found: {}", source_id.0);
        };
        pipeline
            .store()
            .create_collection(collection_name, &[])
            .unwrap();
        pipeline
            .store()
            .replace_collection_members(
                collection_name,
                &[CollectionMemberCandidate {
                    source_id: source_id.clone(),
                    logical_path: "doc.md".into(),
                    source_path: source.path,
                }],
                CollectionSyncReport {
                    member_count: 0,
                    added: 0,
                    removed: 0,
                    unchanged: 0,
                    scanned_roots: 1,
                    max_depth: 32,
                    skipped: Vec::new(),
                },
            )
            .unwrap();
    }

    #[tokio::test]
    async fn ask_stream_token_queue_is_bounded() {
        let (tx, _rx) = mpsc::channel::<Event>(1);

        try_send_stream_event(&tx, Event::default().event("token").data("one")).unwrap();
        let error =
            try_send_stream_event(&tx, Event::default().event("token").data("two")).unwrap_err();

        assert!(error.to_string().contains("not keeping up"));
    }

    #[tokio::test]
    async fn ask_stream_token_callback_propagates_backpressure() {
        let (tx, _rx) = mpsc::channel::<Event>(1);
        let tx_tokens = tx.clone();
        try_send_stream_event(&tx, Event::default().event("token").data("one")).unwrap();

        let on_delta = move |delta: &str| {
            try_send_stream_event(
                &tx_tokens,
                sse_json_event(
                    "token",
                    &AskTokenEvent {
                        text: delta.to_string(),
                    },
                ),
            )?;
            Ok::<_, anyhow::Error>(())
        };

        let error = on_delta("two").unwrap_err();
        assert!(error.to_string().contains("not keeping up"));
    }
}
