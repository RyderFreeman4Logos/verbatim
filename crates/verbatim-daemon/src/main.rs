use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};
use std::convert::Infallible;
use std::fs;
use std::io::Write;
use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures::stream;
use futures::Stream;
use notify::{EventKind, RecommendedWatcher, RecursiveMode, Watcher};
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::mpsc;
use tower_http::cors::CorsLayer;

use verbatim_core::api::{
    AddCollectionRootRequest, AddSourceRequest, AddSourceResponse, AppliedCollectionFilterResponse,
    AskCitationEvent, AskErrorEvent, AskRequest, AskResponse, AskTokenEvent, CheckStaleResponse,
    CitationResponse, CollectionFilterRequest, CollectionFilterResponse, CollectionResponse,
    CollectionResultProvenance, CollectionStatusResponse, CollectionSyncPathRequest,
    CollectionSyncRequest, CollectionSyncResponse, CollectionWatcherResponse,
    CollectionWatcherStatus, CollectionWatcherUpdateRequest, CollectionWatchersStatusResponse,
    ConfigResponse, CreateCollectionRequest, ErrorResponse, EvidenceResponse, HealthResponse,
    ImageArtifactResponse, IngestResponse, ReindexRequest, ReindexResponse,
    RetrieveControlsResponse, RetrieveRequest, RetrieveResponse, RetrieveResultResponse,
    RetrieveTimingResponse, SourceResponse, TaskCreatedResponse, TaskEventsResponse,
    TaskIngestRequest, TaskSummaryResponse, TaskWaitEvent,
};
use verbatim_core::collection::{
    diff_collection_members, validate_collection_name, CollectionIgnoreRules, CollectionMember,
    CollectionMemberCandidate, CollectionRecord, CollectionSyncPathInput,
};
use verbatim_core::config::{
    self, Config, ConfigReloadMetadata, ConfigRestartRequiredKey, RerankConfig, RetrievalConfig,
};
use verbatim_core::embed::OpenAiEmbeddingClient;
use verbatim_core::generate::{
    image_artifact_evidence_id, select_image_attachments, GenerationContext, Generator,
};
use verbatim_core::graphrag::GraphRagService;
use verbatim_core::ingest::{IndexingOutcome, IngestPipeline};
use verbatim_core::ocr::source_ingest_diagnostics;
use verbatim_core::provider::openai_compatible::OpenAiCompatibleReranker;
use verbatim_core::provider::ProviderError;
use verbatim_core::retrieve::{refresh_final_evidence_pack_debug, RetrievalPipeline};
use verbatim_core::store::Store;
use verbatim_core::task::{
    ask_request_metadata, ask_result_metadata, bounded_error, ingest_result_metadata,
    ingest_task_request_metadata_with_queue_claim, reindex_result_metadata,
    reindex_task_request_metadata_with_queue_claim, retrieve_request_metadata,
    retrieve_result_metadata, PhaseTiming, TaskEndpointSummary, TaskId, TaskKind,
    TaskProgressSnapshot, TaskStatus,
};
use verbatim_core::types::{
    CitationRef, EmbeddingProfileId, EvidenceId, EvidenceKind, ImageArtifact, RetrievalDebug,
    RetrievalEvidenceRole, RetrievalResult, SourceId, SourceStatus,
};
use verbatim_core::upstream::{sanitize_text, UpstreamFailureError};

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
    /// Pipeline behind a std Mutex; accessed only inside `spawn_blocking`.
    pipeline: std::sync::Mutex<IngestPipeline>,
    /// Independent task metadata connection so queue operations do not wait for long ingest work.
    task_store: std::sync::Mutex<Store>,
    ingest_queue_active: AtomicBool,
    collection_watcher: CollectionWatcherRuntime,
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

const ASK_STREAM_EVENT_BUFFER: usize = 32;
const TASK_WAIT_EVENT_BUFFER: usize = 16;
const TASK_WAIT_EVENT_LIMIT: usize = 100;
const TASK_WAIT_POLL_INTERVAL: Duration = Duration::from_secs(1);
const FAST_RETRIEVAL_TOP_K: usize = 20;
const DEFAULT_SNIPPET_CHARS: usize = 240;
const CONFIG_RELOAD_DEBOUNCE: Duration = Duration::from_millis(250);
const CONFIG_RELOAD_ERROR_MAX_CHARS: usize = 1024;
const COLLECTION_WATCHER_EVENT_BUFFER: usize = 512;
const COLLECTION_WATCHER_STATUS_ERROR_MAX_CHARS: usize = 1024;

#[derive(Default)]
struct CollectionWatcherRuntime {
    tx: std::sync::Mutex<Option<mpsc::Sender<CollectionWatcherCommand>>>,
    statuses: std::sync::Mutex<HashMap<String, CollectionWatcherStatusState>>,
}

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

fn send_collection_watcher_command(state: &SharedState, command: CollectionWatcherCommand) {
    let tx = match state.collection_watcher.tx.lock() {
        Ok(guard) => guard.clone(),
        Err(error) => {
            tracing::warn!(error = %error, "failed to lock collection watcher command sender");
            None
        }
    };
    if let Some(tx) = tx {
        if let Err(error) = tx.try_send(command) {
            tracing::warn!(error = %error, "collection watcher command queue is full");
        }
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
    force: bool,
    #[serde(default)]
    embedding_profile_id: Option<String>,
    #[serde(default)]
    vectors_only: bool,
    #[serde(default)]
    queue_claimable: Option<bool>,
}

#[derive(Debug, Clone)]
struct IndexingTaskControls {
    source_id: Option<String>,
    force: bool,
    embedding_profile_id: Option<String>,
    vectors_only: bool,
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

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
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

async fn add_source(
    State(state): State<SharedState>,
    Json(req): Json<AddSourceRequest>,
) -> Result<(StatusCode, Json<AddSourceResponse>), (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(&state);
    let result = tokio::task::spawn_blocking(move || {
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let path = PathBuf::from(&req.path);
        pipeline.add_source(&path)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;

    Ok((
        StatusCode::CREATED,
        Json(AddSourceResponse { id: result.0 }),
    ))
}

async fn list_sources(
    State(state): State<SharedState>,
) -> Result<Json<Vec<SourceResponse>>, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(&state);
    let sources = tokio::task::spawn_blocking(move || {
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let sources = pipeline
            .store()
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
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(sources))
}

async fn get_source(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<SourceResponse>, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(&state);
    let id_clone = id.clone();
    let source = tokio::task::spawn_blocking(move || {
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let current_ocr_profile = pipeline.active_ocr_profile();
        let source = pipeline.store().get_source(&SourceId(id_clone))?;
        source
            .map(|source| source_response(pipeline.store(), source, current_ocr_profile.as_ref()))
            .transpose()
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
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

async fn delete_source(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(&state);
    let runtime = tokio::runtime::Handle::current();
    let source_id = SourceId(id.clone());
    tokio::task::spawn_blocking(move || {
        let mut pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        runtime.block_on(pipeline.remove_source(&source_id))
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| source_remove_error(&id, e))?;

    Ok(StatusCode::NO_CONTENT)
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
    let state = Arc::clone(&state);
    let ids = tokio::task::spawn_blocking(move || {
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        pipeline.check_stale()
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(CheckStaleResponse {
        stale: ids.into_iter().map(|id| id.0).collect(),
    }))
}

async fn create_collection(
    State(state): State<SharedState>,
    Json(req): Json<CreateCollectionRequest>,
) -> Result<(StatusCode, Json<CollectionResponse>), (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(&state);
    let response = tokio::task::spawn_blocking(move || {
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let collection = pipeline
            .store()
            .create_collection(&req.name, &req.ignore_patterns)?;
        collection_response(pipeline.store(), collection)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;

    Ok((StatusCode::CREATED, Json(response)))
}

async fn list_collections(
    State(state): State<SharedState>,
) -> Result<Json<Vec<CollectionRecord>>, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(&state);
    let collections = tokio::task::spawn_blocking(move || {
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        pipeline.store().list_collections()
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(collections))
}

async fn get_collection(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<CollectionResponse>, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(&state);
    let name_for_lookup = name.clone();
    let response = tokio::task::spawn_blocking(move || {
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let collection = pipeline.store().get_collection(&name_for_lookup)?;
        collection
            .map(|collection| collection_response(pipeline.store(), collection))
            .transpose()
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
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
    let state_for_store = Arc::clone(&state);
    let name_for_error = name.clone();
    let deleted = tokio::task::spawn_blocking(move || {
        let pipeline = state_for_store
            .pipeline
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        pipeline.store().delete_collection(&name)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
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
) -> Result<Json<CollectionResponse>, (StatusCode, Json<ErrorResponse>)> {
    let state_for_store = Arc::clone(&state);
    let name_for_error = name.clone();
    let response = tokio::task::spawn_blocking(move || {
        let pipeline = state_for_store
            .pipeline
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let path = PathBuf::from(req.path);
        pipeline.store().add_collection_root(&name, &path)?;
        let collection = pipeline
            .store()
            .get_collection(&name)?
            .with_context(|| format!("collection not found: {name}"))?;
        collection_response(pipeline.store(), collection)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
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
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        pipeline.sync_collection(&name, &inputs, req.max_depth)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| collection_error(&name_for_error, e))?;

    Ok(Json(CollectionSyncResponse { report }))
}

async fn collection_status(
    State(state): State<SharedState>,
    Path(name): Path<String>,
) -> Result<Json<CollectionStatusResponse>, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(&state);
    let name_for_lookup = name.clone();
    let status = tokio::task::spawn_blocking(move || {
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        pipeline.store().collection_status(&name_for_lookup)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
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
    let state_for_store = Arc::clone(&state);
    let collections = tokio::task::spawn_blocking(move || {
        let pipeline = state_for_store
            .pipeline
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        pipeline.store().list_collections()
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
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
    let state_for_store = Arc::clone(&state);
    let name_for_lookup = name.clone();
    let collection = tokio::task::spawn_blocking(move || {
        let pipeline = state_for_store
            .pipeline
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        pipeline.store().get_collection(&name_for_lookup)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
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
    let state_for_store = Arc::clone(&state);
    let name_for_lookup = name.clone();
    let collection = tokio::task::spawn_blocking(move || {
        let pipeline = state_for_store
            .pipeline
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let current = pipeline
            .store()
            .get_collection(&name_for_lookup)?
            .with_context(|| format!("collection not found: {name_for_lookup}"))?;
        let auto_index_enabled = req.auto_index_enabled.unwrap_or(current.auto_index_enabled);
        pipeline.store().update_collection_watch_settings(
            &name_for_lookup,
            req.enabled,
            auto_index_enabled,
        )
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
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
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let task_id = TaskId::new();
        let store = state
            .task_store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let task = store.create_task(&task_id, kind, &request)?;
        let payload = queued_event_payload(&store, task)?;
        store.insert_task_event(&task_id, "queued", "task queued", &payload)?;
        Ok::<_, anyhow::Error>(task_id)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))
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
    let state = Arc::clone(state);
    let task_id = task_id.clone();
    tokio::task::spawn_blocking(move || {
        let store = state
            .task_store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let started = store.start_task(&task_id)?;
        if !started {
            return Ok(false);
        }
        store.insert_task_event(&task_id, "started", "task started", &serde_json::json!({}))?;
        Ok::<_, anyhow::Error>(true)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
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

async fn try_mark_ingest_task_started(
    state: &SharedState,
    task_id: &TaskId,
) -> Result<TaskStartOutcome, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(state);
    let task_id = task_id.clone();
    tokio::task::spawn_blocking(move || {
        let store = state
            .task_store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let task = store
            .get_task(&task_id)?
            .with_context(|| format!("task not found: {}", task_id.0))?;
        if task.kind != TaskKind::Ingest {
            bail!("task is not an ingest task: {}", task_id.0);
        }
        if task.status != TaskStatus::Queued {
            return Ok(TaskStartOutcome::NotQueued);
        }
        if store.count_running_tasks(TaskKind::Ingest)? > 0 {
            return Ok(TaskStartOutcome::BlockedByRunningIngest);
        }
        if !store.start_task_if_no_running(&task_id, TaskKind::Ingest)? {
            return Ok(TaskStartOutcome::BlockedByRunningIngest);
        }
        store.insert_task_event(&task_id, "started", "task started", &serde_json::json!({}))?;
        Ok::<_, anyhow::Error>(TaskStartOutcome::Started)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| {
        if e.to_string().contains("task not found") {
            err(StatusCode::NOT_FOUND, e)
        } else {
            err(StatusCode::INTERNAL_SERVER_ERROR, e)
        }
    })
}

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

async fn record_task_event(
    state: &SharedState,
    task_id: &TaskId,
    event_type: &'static str,
    message: impl Into<String>,
    payload: serde_json::Value,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(state);
    let task_id = task_id.clone();
    let message = message.into();
    tokio::task::spawn_blocking(move || {
        let store = state
            .task_store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        store.insert_task_event(&task_id, event_type, &message, &payload)?;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))
}

async fn record_task_span(
    state: &SharedState,
    task_id: &TaskId,
    timing: verbatim_core::task::FinishedPhaseTiming,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(state);
    let task_id = task_id.clone();
    tokio::task::spawn_blocking(move || {
        let store = state
            .task_store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        store.insert_task_span(
            &task_id,
            &timing.phase,
            &timing.started_at,
            timing.duration_ms,
            &timing.metadata,
        )?;
        Ok::<_, anyhow::Error>(())
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))
}

async fn record_task_progress(
    state: &SharedState,
    task_id: &TaskId,
    progress: TaskProgressSnapshot,
) {
    let state = Arc::clone(state);
    let task_id = task_id.clone();
    let task_id_for_log = task_id.clone();
    let result = tokio::task::spawn_blocking(move || {
        let store = state
            .task_store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        store.update_task_progress(&task_id, progress)?;
        Ok::<_, anyhow::Error>(())
    })
    .await;

    match result {
        Ok(Ok(())) => {}
        Ok(Err(err)) => {
            tracing::warn!(task_id = %task_id_for_log.0, error = %err, "failed to persist task progress");
        }
        Err(err) => {
            tracing::warn!(task_id = %task_id_for_log.0, error = %err, "task progress writer panicked");
        }
    }
}

async fn finish_task_success(
    state: &SharedState,
    task_id: &TaskId,
    result: serde_json::Value,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let state_for_task = Arc::clone(state);
    let state_for_queue = Arc::clone(state);
    let task_id = task_id.clone();
    let should_wake_ingest_queue = tokio::task::spawn_blocking(move || {
        let store = state_for_task
            .task_store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let task = store.get_task(&task_id)?;
        let should_wake_ingest_queue = task
            .as_ref()
            .is_some_and(|task| task.kind == TaskKind::Ingest);
        let task_changed = store.finish_task_success(&task_id, &result)?;
        if task_changed {
            store.insert_task_event(&task_id, "succeeded", "task succeeded", &result)?;
        }
        Ok::<_, anyhow::Error>(task_changed && should_wake_ingest_queue)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if should_wake_ingest_queue {
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
    finish_task_failed_with_upstream(state, task_id, &error.error, error.upstream_failure.clone())
        .await
}

async fn finish_task_failed_with_upstream(
    state: &SharedState,
    task_id: &TaskId,
    error_message: &str,
    upstream_failure: Option<serde_json::Value>,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let state_for_task = Arc::clone(state);
    let state_for_queue = Arc::clone(state);
    let task_id = task_id.clone();
    let error_message = bounded_error(error_message);
    let should_wake_ingest_queue = tokio::task::spawn_blocking(move || {
        let store = state_for_task
            .task_store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let task = store.get_task(&task_id)?;
        let upstream_failure = upstream_failure
            .map(|failure| upstream_failure_with_task_context(failure, &task_id, task.as_ref()));
        let should_wake_ingest_queue = task
            .as_ref()
            .is_some_and(|task| task.kind == TaskKind::Ingest);
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
        }
        Ok::<_, anyhow::Error>(task_changed && should_wake_ingest_queue)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    if should_wake_ingest_queue {
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

async fn task_summary_response(
    state: &SharedState,
    task_id: TaskId,
) -> Result<TaskSummaryResponse, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let store = state
            .task_store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let task = store
            .get_task(&task_id)?
            .with_context(|| format!("task not found: {}", task_id.0))?;
        let task = with_queue_details(&store, task)?;
        let spans = store.list_task_spans(&task_id)?;
        Ok::<_, anyhow::Error>(TaskSummaryResponse { task, spans })
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| {
        if e.to_string().contains("task not found") {
            err(StatusCode::NOT_FOUND, e)
        } else {
            err(StatusCode::INTERNAL_SERVER_ERROR, e)
        }
    })
}

async fn task_events_response(
    state: &SharedState,
    task_id: TaskId,
    after: Option<i64>,
    limit: Option<usize>,
) -> Result<TaskEventsResponse, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let store = state
            .task_store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        store
            .get_task(&task_id)?
            .with_context(|| format!("task not found: {}", task_id.0))?;
        let events =
            store.list_task_events(&task_id, after, limit.unwrap_or(TASK_WAIT_EVENT_LIMIT))?;
        Ok::<_, anyhow::Error>(TaskEventsResponse { events })
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
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
        None,
        query.force,
        query.embedding_profile_id.clone(),
        query.vectors_only,
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
    let task_id = create_persisted_task(
        &state,
        TaskKind::Ingest,
        ingest_task_request_metadata_with_queue_claim(
            Some(&id),
            false,
            query.embedding_profile_id.as_deref(),
            query.vectors_only,
            false,
        ),
    )
    .await?;
    execute_ingest_task(
        state,
        task_id,
        Some(id),
        false,
        query.embedding_profile_id.clone(),
        query.vectors_only,
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
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = mpsc::channel::<Event>(ASK_STREAM_EVENT_BUFFER);
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

    Sse::new(stream::unfold(rx, |mut rx: mpsc::Receiver<Event>| async {
        rx.recv().await.map(|event| (Ok(event), rx))
    }))
}

async fn submit_ask_task(
    State(state): State<SharedState>,
    Json(req): Json<AskRequest>,
) -> Result<Json<TaskCreatedResponse>, (StatusCode, Json<ErrorResponse>)> {
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
    let task_id = create_persisted_task(
        &state,
        TaskKind::Ingest,
        ingest_task_request_metadata_with_queue_claim(
            req.source_id.as_deref(),
            req.force,
            req.embedding_profile_id.as_deref(),
            req.vectors_only,
            true,
        ),
    )
    .await?;
    schedule_ingest_queue(Arc::clone(&state));
    Ok(Json(TaskCreatedResponse { task_id: task_id.0 }))
}

async fn submit_reindex_task(
    State(state): State<SharedState>,
    Json(req): Json<ReindexRequest>,
) -> Result<Json<TaskCreatedResponse>, (StatusCode, Json<ErrorResponse>)> {
    let controls = resolve_reindex_controls(req).map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    validate_requested_source_exists(&state, controls.source_id.as_deref()).await?;
    let task_id = create_persisted_task(
        &state,
        TaskKind::Ingest,
        reindex_task_request_metadata_with_queue_claim(
            controls.source_id.as_deref(),
            controls.force,
            controls.embedding_profile_id.as_deref(),
            controls.vectors_only,
            true,
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
    let state = Arc::clone(state);
    let task_id = task_id.clone();
    tokio::task::spawn_blocking(move || {
        let store = state
            .task_store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
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
        Ok::<_, anyhow::Error>(plan)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
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
            ResumableIngestOperation::Ingest => execute_ingest_task(
                Arc::clone(&state),
                task_id.clone(),
                plan.controls.source_id,
                plan.controls.force,
                plan.controls.embedding_profile_id,
                plan.controls.vectors_only,
            )
            .await
            .map(|_| ()),
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

    Sse::new(stream::unfold(rx, |mut rx| async {
        rx.recv().await.map(|event| (Ok(event), rx))
    }))
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
    ensure_task_started(&state, task_id).await?;
    let question = req.question;
    let source_id = req.source_id.map(SourceId);
    let collection_filter = req.collection_filter;
    let embedding_profile_id = parse_embedding_profile_id(
        req.embedding_profile_id.as_deref(),
        &controls.config.embedding.profile_id,
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let query_scope = resolve_query_scope(&state, source_id, collection_filter).await?;

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
    let RetrievedContext {
        results,
        debug,
        source_paths,
    } = prepare_retrieve_context(
        Arc::clone(&state),
        &question,
        query_scope.source_filter.clone(),
        &embedding_profile_id,
        &controls,
    )
    .await?;
    let mut retrieval_progress = timing
        .progress_snapshot()
        .with_counter("dense_candidates", debug.dense_hits.len() as u64, None)
        .with_counter("bm25_candidates", debug.bm25_hits.len() as u64, None)
        .with_counter(
            "retrieval_candidates",
            debug.rrf_fused_hits.len() as u64,
            None,
        )
        .with_counter("evidence", debug.final_evidence_pack.len() as u64, None)
        .with_recent_status(format!(
            "retrieved {} evidence entries",
            debug.final_evidence_pack.len()
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
    let retrieval_timing = timing.finish(serde_json::json!({
        "result_count": results.len(),
        "returned_results": page_len(
            debug.final_evidence_pack.len(),
            controls.limit,
            controls.page_size,
            controls.page,
        ),
        "rerank_enabled": controls.rerank_config.enabled,
        "dense_top_k": controls.retrieval_config.dense_top_k,
        "bm25_top_k": controls.retrieval_config.bm25_top_k,
    }));
    record_task_progress(&state, task_id, retrieval_progress).await;
    record_task_span(&state, task_id, retrieval_timing.clone()).await?;
    record_task_event(
        &state,
        task_id,
        "phase",
        "context retrieval complete",
        serde_json::json!({
            "result_count": results.len(),
            "rerank_enabled": controls.rerank_config.enabled,
        }),
    )
    .await?;

    let response = retrieve_response(RetrieveResponseInput {
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
    });

    finish_task_success(
        &state,
        task_id,
        retrieve_result_metadata(
            response.total_results,
            response.returned_results,
            response.controls.rerank_enabled,
        ),
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

    ensure_task_started(&state, task_id).await?;
    let question = req.question;
    let source_id = req.source_id.map(SourceId);
    let collection_filter = req.collection_filter;
    let config = runtime_config_snapshot(&state)
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?
        .config;
    let embedding_profile_id = parse_embedding_profile_id(
        req.embedding_profile_id.as_deref(),
        &config.embedding.profile_id,
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let show_retrieval = req.show_retrieval;
    let query_scope = resolve_query_scope(&state, source_id, collection_filter).await?;

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
        retrieval_progress.set_counter("dense_candidates", debug.dense_hits.len() as u64, None);
        retrieval_progress.set_counter("bm25_candidates", debug.bm25_hits.len() as u64, None);
        retrieval_progress.set_counter(
            "retrieval_candidates",
            debug.rrf_fused_hits.len() as u64,
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
        timing.finish(serde_json::json!({
            "result_count": results.len(),
            "retrieval_debug": retrieval_debug.is_some(),
        })),
    )
    .await?;
    record_task_event(
        &state,
        task_id,
        "phase",
        "retrieval complete",
        serde_json::json!({ "result_count": results.len() }),
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

    let response = AskResponse {
        answer: gen_result.answer,
        citations: gen_result
            .citations
            .into_iter()
            .map(|citation| {
                citation_response_with_collections(citation, &query_scope.collection_provenance)
            })
            .collect(),
        verified: gen_result.verified,
        retrieval: retrieval_debug,
        context: None,
        collection_filter: query_scope.collection_filter,
    };
    finish_task_success(
        &state,
        task_id,
        ask_result_metadata(
            &response.answer,
            response.citations.len(),
            response.verified,
            response.retrieval.is_some(),
        ),
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

    ensure_task_started(&state, task_id).await?;
    let question = req.question;
    let source_id = req.source_id.map(SourceId);
    let collection_filter = req.collection_filter;
    let config = runtime_config_snapshot(&state)
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?
        .config;
    let embedding_profile_id = parse_embedding_profile_id(
        req.embedding_profile_id.as_deref(),
        &config.embedding.profile_id,
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let show_retrieval = req.show_retrieval;
    let query_scope = resolve_query_scope(&state, source_id, collection_filter).await?;

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
        timing.finish(serde_json::json!({
            "result_count": results.len(),
            "retrieval_debug": retrieval_debug.is_some(),
        })),
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

    if let Some(debug) = retrieval_debug {
        send_stream_event(&tx, sse_json_event("retrieval", &debug)).await?;
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
        limit: None,
        page_size: None,
        page: None,
        fast: false,
        rerank: None,
        dense_top_k: None,
        bm25_top_k: None,
        rerank_top_n: None,
        include_debug: req.show_retrieval,
        include_locator: true,
    }
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
    include_locator: bool,
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
        include_locator: req.include_locator,
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
) -> Result<QueryScope, (StatusCode, Json<ErrorResponse>)> {
    if !collection_filter.has_filters() {
        return Ok(QueryScope {
            source_filter: source_id.clone().map(single_source_set),
            source_id,
            collection_filter: None,
            collection_provenance: HashMap::new(),
        });
    }

    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        resolve_query_scope_from_store(pipeline.store(), source_id, collection_filter)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(collection_filter_error)
}

fn resolve_query_scope_from_store(
    store: &Store,
    source_id: Option<SourceId>,
    requested: CollectionFilterRequest,
) -> Result<QueryScope> {
    let collection_names = collection_filter_names(&requested)?;
    let mut union_source_ids = HashSet::new();
    let mut applied = Vec::new();
    let mut warnings = Vec::new();
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
                    indexed_member_count += 1;
                }
                Some(source) => {
                    stale_member_count += 1;
                    tracing::debug!(
                        collection = %name,
                        source_id = %source.id.0,
                        status = source_status_name(&source.status),
                        "collection member source is not indexed"
                    );
                }
                None => {
                    stale_member_count += 1;
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
        bail!(
            "collection filter requires fresh membership; resolve warnings with `verbatim collection sync <name>` or ingest stale member sources"
        );
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

fn schedule_ingest_queue(state: SharedState) {
    if state.ingest_queue_active.swap(true, Ordering::AcqRel) {
        return;
    }

    tokio::spawn(async move {
        drain_ingest_queue(Arc::clone(&state)).await;
        state.ingest_queue_active.store(false, Ordering::Release);
        match ingest_queue_ready_to_drain(&state).await {
            Ok(true) => schedule_ingest_queue(state),
            Ok(false) => {}
            Err(err) => tracing::error!(error = %err, "failed to inspect ingest queue"),
        }
    });
}

async fn drain_ingest_queue(state: SharedState) {
    loop {
        let task = match claim_startable_ingest_task(&state).await {
            Ok(Some(task)) => task,
            Ok(None) => break,
            Err(err) => {
                tracing::error!(error = %err, "failed to claim queued ingest task");
                break;
            }
        };
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
        };
        let result = if request.operation.as_deref() == Some("reindex") {
            execute_started_reindex_task(Arc::clone(&state), &task_id, controls)
                .await
                .map(|_| ())
        } else {
            execute_started_ingest_task(
                Arc::clone(&state),
                &task_id,
                controls.source_id,
                controls.force,
                controls.embedding_profile_id,
                controls.vectors_only,
            )
            .await
            .map(|_| ())
        };
        if let Err((_, Json(error))) = &result {
            let _ = finish_task_failed_from_response(&state, &task_id, error).await;
        }
    }
}

async fn claim_startable_ingest_task(
    state: &SharedState,
) -> Result<Option<verbatim_core::task::TaskSummary>> {
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let store = state
            .task_store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        claim_next_queue_claimable_ingest_task(&store)
    })
    .await
    .context("join ingest queue claim task")?
}

async fn ingest_queue_ready_to_drain(state: &SharedState) -> Result<bool> {
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let store = state
            .task_store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        Ok::<_, anyhow::Error>(
            store.count_running_tasks(TaskKind::Ingest)? == 0
                && next_queue_claimable_ingest_task(&store)?.is_some(),
        )
    })
    .await
    .context("join ingest queue readiness task")?
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
        },
        queue_claimable: request.queue_claimable.unwrap_or(false),
    }))
}

fn next_queue_claimable_ingest_task(
    store: &Store,
) -> Result<Option<verbatim_core::task::TaskSummary>> {
    for task in store.queued_tasks(TaskKind::Ingest)? {
        if ingest_task_can_be_claimed_by_queue(&task.request) {
            return Ok(Some(task));
        }
    }
    Ok(None)
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

fn ingest_task_can_be_claimed_by_queue(request: &serde_json::Value) -> bool {
    match serde_json::from_value::<PersistedIngestRequest>(request.clone()) {
        Ok(request) => {
            request.ingest_request_version != Some(1) || request.queue_claimable.unwrap_or(true)
        }
        Err(_) => true,
    }
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
        bail!("embedding_profile_id is supported for vectors-only builds; set [embedding].profile_id for parse ingest");
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
        bail!("stale vector-only reindex is not supported; rebuild all vectors or run stale reindex without vectors_only");
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
    let state = Arc::clone(state);
    let found = tokio::task::spawn_blocking(move || {
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        pipeline.store().get_source(&lookup_id)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
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
    source_id: Option<String>,
    force: bool,
    embedding_profile_id: Option<String>,
    vectors_only: bool,
) -> Result<IngestResponse, (StatusCode, Json<ErrorResponse>)> {
    let result = async {
        ensure_ingest_task_started(&state, &task_id).await?;
        execute_started_ingest_task(
            Arc::clone(&state),
            &task_id,
            source_id,
            force,
            embedding_profile_id,
            vectors_only,
        )
        .await
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
    source_id: Option<String>,
    force: bool,
    embedding_profile_id: Option<String>,
    vectors_only: bool,
) -> Result<IngestResponse, (StatusCode, Json<ErrorResponse>)> {
    let controls = IndexingTaskControls {
        source_id,
        force,
        embedding_profile_id,
        vectors_only,
    };
    let (outcome, profile_id) =
        run_indexing_operation(Arc::clone(&state), task_id, &controls, "ingest").await?;
    let response = IngestResponse {
        ingested: outcome.source_count,
    };
    finish_task_success(
        &state,
        task_id,
        ingest_result_metadata(response.ingested, &outcome.embedding_cache),
    )
    .await?;
    tracing::debug!(
        task_id = %task_id.0,
        embedding_profile_id = %profile_id,
        "ingest task completed"
    );
    Ok(response)
}

async fn execute_reindex_task(
    state: SharedState,
    task_id: TaskId,
    controls: IndexingTaskControls,
) -> Result<ReindexResponse, (StatusCode, Json<ErrorResponse>)> {
    let result = async {
        ensure_ingest_task_started(&state, &task_id).await?;
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
    let state2 = Arc::clone(&state);
    let task_id2 = task_id.clone();
    let profile_id_for_task = profile_id.clone();
    let source_id = controls.source_id.clone();
    let source_id_for_error = controls.source_id.clone();
    let force = controls.force;
    let vectors_only = controls.vectors_only;
    let runtime = tokio::runtime::Handle::current();
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
    let outcome = tokio::task::spawn_blocking(move || {
        let mut pipeline = state2.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        if vectors_only {
            let source_filter = source_id.as_ref().map(|id| SourceId(id.clone()));
            return runtime.block_on(
                pipeline.build_embedding_profile(&profile_id_for_task, source_filter.as_ref()),
            );
        }
        match source_id {
            Some(id) => {
                let embedding_cache =
                    runtime.block_on(pipeline.ingest_source_with_task(&SourceId(id), &task_id2))?;
                Ok::<_, anyhow::Error>(IndexingOutcome {
                    source_count: 1,
                    embedding_cache,
                })
            }
            None => runtime.block_on(pipeline.ingest_all_with_task(force, &task_id2)),
        }
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| indexing_operation_error(source_id_for_error.as_deref(), e))?;
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

fn indexing_operation_error(
    source_id: Option<&str>,
    error: anyhow::Error,
) -> (StatusCode, Json<ErrorResponse>) {
    let status = match source_id {
        Some(source_id) if is_source_not_found_error(source_id, &error) => StatusCode::NOT_FOUND,
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    err(status, error)
}

async fn cancel_task_record(
    state: &SharedState,
    task_id: &TaskId,
) -> Result<bool, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(state);
    let task_id = task_id.clone();
    tokio::task::spawn_blocking(move || {
        let store = state
            .task_store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        store
            .get_task(&task_id)?
            .with_context(|| format!("task not found: {}", task_id.0))?;
        let changed = store.cancel_task(&task_id)?;
        if changed {
            store.insert_task_event(
                &task_id,
                "cancelled",
                "task cancelled",
                &serde_json::json!({}),
            )?;
        }
        Ok::<_, anyhow::Error>(changed)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| {
        if e.to_string().contains("task not found") {
            err(StatusCode::NOT_FOUND, e)
        } else {
            err(StatusCode::INTERNAL_SERVER_ERROR, e)
        }
    })
}

async fn task_wait_snapshot(
    state: &SharedState,
    task_id: TaskId,
    after: Option<i64>,
    limit: usize,
) -> Result<TaskWaitEvent, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let store = state
            .task_store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let task = store
            .get_task(&task_id)?
            .with_context(|| format!("task not found: {}", task_id.0))?;
        let task = with_queue_details(&store, task)?;
        let events = store.list_task_events(&task_id, after, limit)?;
        let spans = if task.status.is_terminal() {
            store.list_task_spans(&task_id)?
        } else {
            Vec::new()
        };
        let terminal = task.status.is_terminal();
        Ok::<_, anyhow::Error>(TaskWaitEvent {
            task,
            events,
            spans,
            terminal,
        })
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
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

async fn prepare_retrieve_context(
    state: SharedState,
    question: &str,
    source_filter: Option<HashSet<SourceId>>,
    embedding_profile_id: &EmbeddingProfileId,
    controls: &EffectiveRetrieveControls,
) -> Result<RetrievedContext, (StatusCode, Json<ErrorResponse>)> {
    let state2 = Arc::clone(&state);
    let question2 = question.to_string();
    let embedding_profile_id = embedding_profile_id.clone();
    let controls = controls.clone();
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let mut pipeline = state2.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        pipeline.select_embedding_profile(&embedding_profile_id)?;
        let lexical_index = pipeline.lexical_index();
        let embed_client = OpenAiEmbeddingClient::new(&controls.config.embedding);
        let reranker = OpenAiCompatibleReranker::from_config(&controls.rerank_config);
        let retrieval = RetrievalPipeline::new_with_graph(
            pipeline.vector_index(),
            &lexical_index,
            pipeline.store(),
            &embed_client,
            &controls.retrieval_config,
            &controls.config.graph,
        )
        .require_embedding_profile(&embedding_profile_id)
        .with_qdrant_search(&controls.config.qdrant)
        .with_reranker(&controls.rerank_config, &reranker);
        let source_filter_ref = source_filter.as_ref();
        let (mut results, mut debug) = runtime
            .block_on(retrieval.search_source_set_with_debug(&question2, source_filter_ref))?;
        if controls.config.graph.global_search.enabled && source_filter_ref.is_none() {
            let global_results =
                GraphRagService::new(pipeline.store(), &controls.config.graph.global_search)
                    .global_search_results(&question2, None)?;
            let mut debug_option = Some(debug);
            prepend_global_results(&mut results, global_results, &mut debug_option);
            debug = debug_option.unwrap_or_else(empty_retrieval_debug);
        }
        let source_paths = source_paths_for_results(&results, pipeline.store())?;
        Ok::<_, anyhow::Error>(RetrievedContext {
            results,
            debug,
            source_paths,
        })
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))
}

fn empty_retrieval_debug() -> RetrievalDebug {
    RetrievalDebug {
        query_embedding_latency_ms: None,
        bm25_hits: Vec::new(),
        dense_hits: Vec::new(),
        rrf_fused_hits: Vec::new(),
        graph_expanded_hits: Vec::new(),
        reranker: verbatim_core::types::RetrievalRerankDebug::disabled(),
        final_evidence_pack: Vec::new(),
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
    let state2 = Arc::clone(&state);
    let question2 = question.to_string();
    let embedding_profile_id = embedding_profile_id.clone();
    let config = config.clone();
    let runtime = tokio::runtime::Handle::current();
    let (results, generation_context, retrieval_debug) = tokio::task::spawn_blocking(move || {
        let mut pipeline = state2.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        pipeline.select_embedding_profile(&embedding_profile_id)?;
        let lexical_index = pipeline.lexical_index();
        let embed_client = OpenAiEmbeddingClient::new(&config.embedding);
        let reranker = OpenAiCompatibleReranker::from_config(&config.rerank);
        let retrieval = RetrievalPipeline::new_with_graph(
            pipeline.vector_index(),
            &lexical_index,
            pipeline.store(),
            &embed_client,
            &config.retrieval,
            &config.graph,
        )
        .require_embedding_profile(&embedding_profile_id)
        .with_qdrant_search(&config.qdrant)
        .with_reranker(&config.rerank, &reranker);
        let source_filter_ref = source_filter.as_ref();
        let (mut results, mut retrieval_debug) = if show_retrieval {
            let (results, debug) = runtime
                .block_on(retrieval.search_source_set_with_debug(&question2, source_filter_ref))?;
            (results, Some(debug))
        } else {
            let results =
                runtime.block_on(retrieval.search_source_set(&question2, source_filter_ref))?;
            (results, None)
        };
        if config.graph.global_search.enabled && source_filter_ref.is_none() {
            let global_results =
                GraphRagService::new(pipeline.store(), &config.graph.global_search)
                    .global_search_results(&question2, None)?;
            prepend_global_results(&mut results, global_results, &mut retrieval_debug);
        }
        let image_artifacts = collect_image_artifacts_for_results(&results, pipeline.store())?;
        let image_attachments = select_image_attachments(
            &results,
            &image_artifacts,
            &config.chat.vision_attachments,
            |artifact| read_image_attachment_bytes(&state2.data_dir, artifact),
        )?;
        Ok::<_, anyhow::Error>((
            results,
            GenerationContext::new(image_artifacts, image_attachments),
            retrieval_debug,
        ))
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok((results, generation_context, retrieval_debug))
}

fn prepend_global_results(
    results: &mut Vec<RetrievalResult>,
    mut global_results: Vec<RetrievalResult>,
    retrieval_debug: &mut Option<RetrievalDebug>,
) {
    if global_results.is_empty() {
        return;
    }

    global_results.extend(std::mem::take(results));
    *results = global_results;
    renumber_result_ranks(results);
    if let Some(debug) = retrieval_debug.as_mut() {
        refresh_final_evidence_pack_debug(debug, results);
    }
}

fn renumber_result_ranks(results: &mut [RetrievalResult]) {
    for (idx, result) in results.iter_mut().enumerate() {
        result.provenance.result_rank = idx + 1;
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

fn retrieve_response(input: RetrieveResponseInput) -> RetrieveResponse {
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
    let total_results = debug.final_evidence_pack.len();
    let returned_results = page_len(
        total_results,
        controls.limit,
        controls.page_size,
        controls.page,
    );
    let results_page = retrieve_result_page(RetrieveResultPageInput {
        results: &results,
        debug: &debug,
        source_paths: &source_paths,
        collection_provenance: &collection_provenance,
        limit: controls.limit,
        page_size: controls.page_size,
        page: controls.page,
        include_locator: controls.include_locator,
    });
    let debug = controls.include_debug.then_some(debug);

    RetrieveResponse {
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
    }
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
    results: &'a [RetrievalResult],
    debug: &'a RetrievalDebug,
    source_paths: &'a HashMap<String, String>,
    collection_provenance: &'a HashMap<String, Vec<CollectionResultProvenance>>,
    limit: usize,
    page_size: usize,
    page: usize,
    include_locator: bool,
}

fn retrieve_result_page(input: RetrieveResultPageInput<'_>) -> Vec<RetrieveResultResponse> {
    let RetrieveResultPageInput {
        results,
        debug,
        source_paths,
        collection_provenance,
        limit,
        page_size,
        page,
        include_locator,
    } = input;
    let start = page_start(page, page_size);
    let end = debug.final_evidence_pack.len().min(limit);
    if start >= end {
        return Vec::new();
    }

    debug
        .final_evidence_pack
        .iter()
        .enumerate()
        .skip(start)
        .take(end - start)
        .take(page_size)
        .map(|(index, entry)| RetrieveResultResponse {
            index,
            rank: index + 1,
            label: entry.label.clone(),
            evidence_id: entry.evidence_id.0.clone(),
            source_id: entry.source_id.0.clone(),
            source_path: source_paths.get(&entry.source_id.0).cloned(),
            collections: collection_provenance
                .get(&entry.source_id.0)
                .cloned()
                .unwrap_or_default(),
            chunk_id: entry.chunk_id.0.clone(),
            kind: evidence_kind_name(entry.kind).to_string(),
            role: retrieval_role_name(entry.role).to_string(),
            score: entry.score,
            locator: entry.locator.display.clone(),
            structured_locator: include_locator.then(|| entry.locator.structured.clone()),
            provenance: include_locator.then(|| entry.provenance.clone()),
            derived_from: entry.derived_from.as_ref().map(|id| id.0.clone()),
            snippet: evidence_snippet(results, &entry.evidence_id.0),
        })
        .collect()
}

fn page_start(page: usize, page_size: usize) -> usize {
    page.saturating_sub(1).saturating_mul(page_size)
}

fn evidence_snippet(results: &[RetrievalResult], evidence_id: &str) -> String {
    let text = results
        .iter()
        .flat_map(|result| &result.evidence_units)
        .find(|evidence| evidence.id.0 == evidence_id)
        .map(|evidence| evidence.text.as_str())
        .unwrap_or_default();
    compact_snippet(text, DEFAULT_SNIPPET_CHARS)
}

fn compact_snippet(text: &str, max_chars: usize) -> String {
    let normalized = text.split_whitespace().collect::<Vec<_>>().join(" ");
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
    let state = Arc::clone(&state);
    let eid_clone = eid.clone();
    let (evidence, image_artifact) = tokio::task::spawn_blocking(move || {
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let evidence = pipeline.store().get_evidence(&EvidenceId(eid_clone))?;
        let image_artifact = match &evidence {
            Some(eu) => {
                let direct = pipeline.store().get_image_artifact_by_evidence(&eu.id)?;
                match (direct, &eu.derived_from) {
                    (Some(artifact), _) => Some(artifact),
                    (None, Some(source_evidence_id)) => pipeline
                        .store()
                        .get_image_artifact_by_evidence(source_evidence_id)?,
                    (None, None) => None,
                }
            }
            None => None,
        };
        Ok::<_, anyhow::Error>((evidence, image_artifact))
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    match evidence {
        Some(eu) => Ok(Json(EvidenceResponse {
            kind: evidence_kind_name(eu.kind).to_string(),
            id: eu.id.0,
            source_id: eu.source_id.0,
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

fn sse_error_event(status: StatusCode, error: String) -> Event {
    sse_json_event(
        "error",
        &AskErrorEvent {
            status: Some(status.as_u16()),
            error,
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
        update_collection_watcher_status(state, &name, |status| {
            status.pending_event_count = 0;
        });
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
                tracing::warn!(collection = %name, error = %error, "collection watcher maintenance failed");
                record_collection_watcher_error(state, &name, error);
            }
        }
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
    let state_for_store = Arc::clone(state);
    let plan = tokio::task::spawn_blocking(move || {
        let pipeline = state_for_store
            .pipeline
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        let collections = pipeline.store().list_collections()?;
        let mut plan = CollectionWatchPlan::default();
        for collection in collections {
            let ignored_by_config =
                !watcher_config.enabled || ignored_collections.contains(&collection.name);
            let mut watched_root_count = 0_usize;
            if collection.watch_enabled && !ignored_by_config {
                let roots = pipeline.store().list_collection_roots(&collection.name)?;
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
            update_collection_watcher_status(&state_for_store, &collection.name, |status| {
                status.active =
                    collection.watch_enabled && !ignored_by_config && watched_root_count > 0;
                status.ignored_by_config = ignored_by_config;
                status.watched_root_count = watched_root_count;
            });
        }
        Ok::<_, anyhow::Error>(plan)
    })
    .await
    .context("join collection watch plan task")??;
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
        let mut pipeline = state_for_sync
            .pipeline
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
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
        let mut source_ids_to_ingest = diff
            .added
            .iter()
            .map(|candidate| candidate.source_id.clone())
            .chain(
                stale
                    .into_iter()
                    .filter(|source_id| member_source_ids.contains(source_id)),
            )
            .collect::<BTreeSet<_>>();
        for removed in &diff.removed {
            if !removed.source_path.exists()
                && pipeline.store().get_source(&removed.source_id)?.is_some()
            {
                runtime.block_on(pipeline.remove_source(&removed.source_id))?;
                source_ids_to_ingest.remove(&removed.source_id);
            }
        }
        Ok(CollectionMaintenanceSyncOutcome {
            added: report.added,
            removed: report.removed,
            unchanged: report.unchanged,
            auto_index_enabled: collection.auto_index_enabled,
            source_ids_to_ingest: source_ids_to_ingest.into_iter().collect(),
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
    for source_id in sync_outcome
        .source_ids_to_ingest
        .into_iter()
        .take(watcher_config.max_queued_tasks.max(1))
    {
        if source_has_pending_ingest_task(state, &source_id).await? {
            continue;
        }
        let task_id = create_persisted_task(
            state,
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
        .map_err(|(_, Json(error))| anyhow::anyhow!(error.error))?;
        outcome.queued_task_ids.push(task_id.0);
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
    source_ids_to_ingest: Vec<SourceId>,
}

async fn source_has_pending_ingest_task(state: &SharedState, source_id: &SourceId) -> Result<bool> {
    let state = Arc::clone(state);
    let source_id = source_id.clone();
    tokio::task::spawn_blocking(move || {
        let store = state
            .task_store
            .lock()
            .map_err(|e| anyhow::anyhow!("{e}"))?;
        for task in store.queued_tasks(TaskKind::Ingest)? {
            let request: PersistedIngestRequest = match serde_json::from_value(task.request) {
                Ok(request) => request,
                Err(_) => continue,
            };
            if request.operation.as_deref().unwrap_or("ingest") == "ingest"
                && request.source_id.as_deref() == Some(source_id.0.as_str())
            {
                return Ok(true);
            }
        }
        Ok(false)
    })
    .await
    .context("join pending ingest task lookup")?
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
        Json(ErrorResponse {
            error: format!("{e:#}"),
            upstream_failure,
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
    if collection_watcher_plan_changed {
        send_collection_watcher_command(state, CollectionWatcherCommand::Refresh);
    }
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

async fn shutdown_signal() {
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

    tokio::select! {
        () = ctrl_c => {},
        () = terminate => {},
    }

    tracing::info!("shutdown signal received");
}

// ---------------------------------------------------------------------------
// Main
// ---------------------------------------------------------------------------

#[tokio::main]
async fn main() -> Result<()> {
    if write_version_if_requested(std::env::args().skip(1), &mut std::io::stdout())? {
        return Ok(());
    }

    run_daemon().await
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

async fn run_daemon() -> Result<()> {
    tracing_subscriber::fmt::init();

    let config_path = config::config_path();
    let config = Config::load_from(&config_path).context("failed to load config")?;
    let data_dir = config::data_dir(&config);
    let pipeline = IngestPipeline::new(&config, &data_dir)?;
    let task_store = Store::new(&data_dir.join("verbatim.db"))?;

    let bind_addr = config.daemon.bind.clone();

    let state: SharedState = Arc::new(AppState {
        pipeline: std::sync::Mutex::new(pipeline),
        task_store: std::sync::Mutex::new(task_store),
        ingest_queue_active: AtomicBool::new(false),
        collection_watcher: CollectionWatcherRuntime::default(),
        runtime_config: std::sync::RwLock::new(RuntimeConfigState {
            config,
            reload: initial_reload_metadata(&config_path),
        }),
        config_path,
        data_dir,
    });
    let _config_watcher = start_config_watcher(Arc::clone(&state))?;
    let _collection_watcher = start_collection_watcher(Arc::clone(&state))?;
    schedule_ingest_queue(Arc::clone(&state));

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/config", get(get_config))
        .route("/api/sources", post(add_source))
        .route("/api/sources", get(list_sources))
        .route("/api/sources/{id}", get(get_source))
        .route("/api/sources/{id}", delete(delete_source))
        .route("/api/sources/check", post(check_stale))
        .route("/api/collections", post(create_collection))
        .route("/api/collections", get(list_collections))
        .route("/api/collections/{name}", get(get_collection))
        .route("/api/collections/{name}", delete(delete_collection))
        .route("/api/collections/{name}/roots", post(add_collection_root))
        .route("/api/collections/{name}/sync", post(sync_collection))
        .route("/api/collections/{name}/status", get(collection_status))
        .route(
            "/api/collections/watchers/status",
            get(list_collection_watcher_statuses),
        )
        .route(
            "/api/collections/{name}/watcher",
            get(collection_watcher_status).put(update_collection_watcher),
        )
        .route("/api/ingest", post(ingest_all))
        .route("/api/ingest/{id}", post(ingest_one))
        .route("/api/reindex", post(reindex))
        .route("/api/ask", post(ask))
        .route("/api/ask/stream", post(ask_stream))
        .route("/api/retrieve", post(retrieve))
        .route("/api/tasks/ask", post(submit_ask_task))
        .route("/api/tasks/ingest", post(submit_ingest_task))
        .route("/api/tasks/reindex", post(submit_reindex_task))
        .route("/api/tasks/{id}", get(show_task))
        .route("/api/tasks/{id}/events", get(list_task_events_handler))
        .route("/api/tasks/{id}/wait", get(wait_task))
        .route("/api/tasks/{id}/cancel", post(cancel_task_handler))
        .route("/api/tasks/{id}/resume", post(resume_task_handler))
        .route("/api/evidence/{eid}", get(get_evidence))
        .layer(CorsLayer::permissive())
        .with_state(state);

    tracing::info!(bind = %bind_addr, "starting verbatim daemon");

    let listener = TcpListener::bind(&bind_addr)
        .await
        .with_context(|| format!("failed to bind {bind_addr}"))?;

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await
        .context("server error")?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use verbatim_core::types::{
        Chunk, ChunkId, ChunkType, EmbeddingCacheStats, EvidenceKind, EvidenceUnit,
        RetrievalEvidenceRole, RetrievalProvenance, SourceLocator,
    };

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

    #[test]
    fn ask_request_defaults_retrieval_debug_off() {
        let req: AskRequest =
            serde_json::from_value(serde_json::json!({"question": "What is cited?"})).unwrap();

        assert_eq!(req.question, "What is cited?");
        assert!(req.source_id.is_none());
        assert!(req.collection_filter.is_empty());
        assert!(!req.show_retrieval);
        assert!(!req.context_only);
    }

    #[test]
    fn ask_response_omits_retrieval_when_debug_is_off() {
        let response = AskResponse {
            answer: "Answer [E1].".into(),
            citations: Vec::new(),
            verified: false,
            retrieval: None,
            context: None,
            collection_filter: None,
        };

        let encoded = serde_json::to_value(response).unwrap();

        assert_eq!(
            encoded,
            serde_json::json!({
                "answer": "Answer [E1].",
                "citations": [],
                "verified": false,
            })
        );
        assert!(encoded.get("collection_filter").is_none());
    }

    #[test]
    fn ask_response_includes_structured_retrieval_when_requested() {
        let response = AskResponse {
            answer: "Answer [E1].".into(),
            citations: Vec::new(),
            verified: false,
            retrieval: Some(RetrievalDebug {
                query_embedding_latency_ms: None,
                bm25_hits: Vec::new(),
                dense_hits: Vec::new(),
                rrf_fused_hits: Vec::new(),
                graph_expanded_hits: Vec::new(),
                reranker: verbatim_core::types::RetrievalRerankDebug::disabled(),
                final_evidence_pack: Vec::new(),
            }),
            context: None,
            collection_filter: None,
        };

        let encoded = serde_json::to_string(&response).unwrap();

        assert!(encoded.contains("retrieval"));
        assert!(encoded.contains("bm25_hits"));
        assert!(encoded.contains("final_evidence_pack"));
        assert!(encoded.contains("disabled"));
        assert!(!encoded.contains("api_key"));
        assert!(!encoded.contains("secret full raw source text"));
    }

    #[test]
    fn prepended_global_results_refresh_retrieval_debug_final_pack() {
        let local = test_retrieval_result(1, "local-chunk", "ev-local", EvidenceKind::Text);
        let global = test_retrieval_result(
            1,
            "graphrag:report-chunk:community-test",
            "graphrag:report:community-test",
            EvidenceKind::Generated,
        );
        let mut results = vec![local];
        let mut debug = Some(RetrievalDebug {
            query_embedding_latency_ms: None,
            bm25_hits: Vec::new(),
            dense_hits: Vec::new(),
            rrf_fused_hits: Vec::new(),
            graph_expanded_hits: Vec::new(),
            reranker: verbatim_core::types::RetrievalRerankDebug::disabled(),
            final_evidence_pack: Vec::new(),
        });

        prepend_global_results(&mut results, vec![global], &mut debug);

        assert_eq!(
            results[0].chunk_id.0,
            "graphrag:report-chunk:community-test"
        );
        assert_eq!(results[0].provenance.result_rank, 1);
        assert_eq!(results[1].provenance.result_rank, 2);

        let final_pack = debug.unwrap().final_evidence_pack;
        assert_eq!(final_pack.len(), 2);
        assert_eq!(final_pack[0].label, "E1");
        assert_eq!(
            final_pack[0].evidence_id.0,
            "graphrag:report:community-test"
        );
        assert_eq!(final_pack[0].role, RetrievalEvidenceRole::Generated);
        assert_eq!(final_pack[0].result_rank, 1);
        assert_eq!(final_pack[1].label, "E2");
        assert_eq!(final_pack[1].evidence_id.0, "ev-local");
        assert_eq!(final_pack[1].role, RetrievalEvidenceRole::OriginalText);
        assert_eq!(final_pack[1].result_rank, 2);
    }

    #[test]
    fn retrieve_response_pages_context_pack_without_full_locator_by_default() {
        let results = vec![
            test_retrieval_result(1, "chunk-1", "ev-1", EvidenceKind::Text),
            test_retrieval_result(2, "chunk-2", "ev-2", EvidenceKind::Text),
        ];
        let mut debug = empty_retrieval_debug();
        refresh_final_evidence_pack_debug(&mut debug, &results);

        let response = retrieve_response(RetrieveResponseInput {
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
                include_locator: false,
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
            body.error,
            "source not found: __missing_source_smoke_retest__"
        );
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

        let status = delete_source(State(Arc::clone(&state)), Path(source_id.0.clone()))
            .await
            .unwrap();

        assert_eq!(status, StatusCode::NO_CONTENT);
        let pipeline = state.pipeline.lock().unwrap();
        assert!(pipeline.store().get_source(&source_id).unwrap().is_none());
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
        assert_eq!(with_root.roots.len(), 1);

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
        let pipeline = state.pipeline.lock().unwrap();
        assert_eq!(
            pipeline
                .store()
                .list_collection_members("articles")
                .unwrap()
                .len(),
            1
        );
        drop(pipeline);
        let store = state.task_store.lock().unwrap();
        let queued = store.queued_tasks(TaskKind::Ingest).unwrap();
        assert!(queued.iter().any(|task| {
            task.request
                .get("source_id")
                .and_then(serde_json::Value::as_str)
                .is_some()
        }));
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
                include_debug: false,
                include_locator: false,
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
                include_debug: false,
                include_locator: true,
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
            }),
        )
        .await
        .unwrap()
        .0;

        assert!(response.answer.is_empty());
        assert!(response.citations.is_empty());
        assert!(!response.verified);
        assert!(response.retrieval.is_none());
        let context = response.context.expect("context pack");
        assert_eq!(context.source_id.as_deref(), Some(source_id.0.as_str()));
        assert_eq!(context.returned_results, 1);
        assert_eq!(context.results[0].label, "E1");
        assert!(context.results[0]
            .snippet
            .contains("Beta retrieval evidence"));
        assert!(context.results[0].structured_locator.is_some());
        assert!(model_server.embedding_requests() >= 2);
        assert_eq!(model_server.chat_requests(), 0);
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
            body.error,
            "source not found: __missing_source_smoke_retest__"
        );
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
        wait_for_ingest_queue_idle(&state).await;

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
        wait_for_ingest_queue_idle(&state).await;

        let queued = task_summary_response(&state, task_id).await.unwrap().task;
        assert_eq!(queued.status, TaskStatus::Queued);
        assert_eq!(queued.request["operation"], "reindex");
        assert_eq!(queued.request["force"], true);
        assert!(queued.request["source_id"].is_null());
    }

    #[tokio::test]
    async fn queued_background_ingest_drains_after_running_ingest_finishes() {
        let test_dir = TestDir::new("ingest-queue-drain");
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, test_dir.path()).unwrap();
        let state = test_state(config, test_dir.path(), pipeline);
        let (running_id, queued_id) = blocked_ingest_pair(&state).await;
        assert_queued_ingest_waits_for_running(&state, &queued_id).await;
        schedule_ingest_queue(Arc::clone(&state));
        wait_for_ingest_queue_idle(&state).await;

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
        schedule_ingest_queue(Arc::clone(&state));
        wait_for_ingest_queue_idle(&state).await;

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
        assert_eq!(resumed.task.request["source_id"], "src-1");
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

        schedule_ingest_queue(Arc::clone(&state));
        wait_for_ingest_queue_idle(&state).await;

        let foreground = task_summary_response(&state, foreground_id.clone())
            .await
            .unwrap()
            .task;
        assert_eq!(foreground.status, TaskStatus::Queued);
        let response = execute_ingest_task(
            Arc::clone(&state),
            foreground_id.clone(),
            None,
            false,
            None,
            false,
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

    fn test_state(config: Config, data_dir: &FsPath, pipeline: IngestPipeline) -> SharedState {
        test_state_with_config_path(config, data_dir, pipeline, data_dir.join("config.toml"))
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
        Arc::new(AppState {
            pipeline: std::sync::Mutex::new(pipeline),
            task_store: std::sync::Mutex::new(Store::new(&data_dir.join("verbatim.db")).unwrap()),
            ingest_queue_active: AtomicBool::new(false),
            collection_watcher: CollectionWatcherRuntime::default(),
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

    async fn wait_for_ingest_queue_idle(state: &SharedState) {
        tokio::time::timeout(Duration::from_secs(2), async {
            while state.ingest_queue_active.load(Ordering::Acquire) {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .unwrap();
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

    struct MockModelServer {
        base_url: String,
        embedding_requests: Arc<AtomicUsize>,
        chat_requests: Arc<AtomicUsize>,
        handle: tokio::task::JoinHandle<()>,
    }

    impl MockModelServer {
        async fn start(dimension: usize) -> Self {
            let state = MockModelState {
                dimension,
                embedding_requests: Arc::new(AtomicUsize::new(0)),
                chat_requests: Arc::new(AtomicUsize::new(0)),
            };
            let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
            let addr = listener.local_addr().unwrap();
            let app = Router::new()
                .route("/v1/embeddings", post(mock_embeddings))
                .route("/v1/chat/completions", post(mock_forbidden_chat))
                .with_state(state.clone());
            let handle = tokio::spawn(async move {
                let _ = axum::serve(listener, app).await;
            });

            Self {
                base_url: format!("http://{addr}/v1"),
                embedding_requests: state.embedding_requests,
                chat_requests: state.chat_requests,
                handle,
            }
        }

        fn embedding_requests(&self) -> usize {
            self.embedding_requests.load(Ordering::SeqCst)
        }

        fn chat_requests(&self) -> usize {
            self.chat_requests.load(Ordering::SeqCst)
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
        embedding_requests: Arc<AtomicUsize>,
        chat_requests: Arc<AtomicUsize>,
    }

    async fn mock_embeddings(
        State(state): State<MockModelState>,
        Json(payload): Json<serde_json::Value>,
    ) -> Json<serde_json::Value> {
        state.embedding_requests.fetch_add(1, Ordering::SeqCst);
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

    async fn mock_forbidden_chat(
        State(state): State<MockModelState>,
    ) -> (StatusCode, Json<serde_json::Value>) {
        state.chat_requests.fetch_add(1, Ordering::SeqCst);
        (
            StatusCode::INTERNAL_SERVER_ERROR,
            Json(serde_json::json!({ "error": "chat must not be called by retrieve" })),
        )
    }

    fn retrieve_test_config(model_base_url: &str) -> Config {
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
