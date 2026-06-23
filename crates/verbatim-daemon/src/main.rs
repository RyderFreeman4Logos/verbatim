use std::collections::{HashMap, HashSet};
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
    AddSourceRequest, AddSourceResponse, AskCitationEvent, AskErrorEvent, AskRequest, AskResponse,
    AskTokenEvent, CheckStaleResponse, CitationResponse, ConfigResponse, ErrorResponse,
    EvidenceResponse, HealthResponse, ImageArtifactResponse, IngestResponse, ReindexRequest,
    ReindexResponse, RetrieveControlsResponse, RetrieveRequest, RetrieveResponse,
    RetrieveResultResponse, RetrieveTimingResponse, SourceResponse, TaskCreatedResponse,
    TaskEventsResponse, TaskIngestRequest, TaskSummaryResponse, TaskWaitEvent,
};
use verbatim_core::config::{
    self, Config, ConfigReloadMetadata, ConfigRestartRequiredKey, RerankConfig, RetrievalConfig,
};
use verbatim_core::embed::OpenAiEmbeddingClient;
use verbatim_core::generate::{
    image_artifact_evidence_id, select_image_attachments, GenerationContext, Generator,
};
use verbatim_core::graphrag::GraphRagService;
use verbatim_core::ingest::IngestPipeline;
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
    RetrievalEvidenceRole, RetrievalResult, SourceId,
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

#[derive(Debug, Deserialize)]
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
        let task_changed = store.finish_task_failed(&task_id, &error_message)?;
        if task_changed {
            let mut payload = serde_json::json!({ "error": error_message });
            if let Some(upstream_failure) = upstream_failure {
                payload["upstream_failure"] = upstream_failure;
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
    let source_filter = req.source_id.map(SourceId);
    let embedding_profile_id = parse_embedding_profile_id(
        req.embedding_profile_id.as_deref(),
        &controls.config.embedding.profile_id,
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;

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
        source_filter.clone(),
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
        source_filter,
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
    let source_filter = req.source_id.map(SourceId);
    let config = runtime_config_snapshot(&state)
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?
        .config;
    let embedding_profile_id = parse_embedding_profile_id(
        req.embedding_profile_id.as_deref(),
        &config.embedding.profile_id,
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let show_retrieval = req.show_retrieval;

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
        source_filter,
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
            .map(citation_response)
            .collect(),
        verified: gen_result.verified,
        retrieval: retrieval_debug,
        context: None,
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
    let source_filter = req.source_id.map(SourceId);
    let config = runtime_config_snapshot(&state)
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?
        .config;
    let embedding_profile_id = parse_embedding_profile_id(
        req.embedding_profile_id.as_deref(),
        &config.embedding.profile_id,
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let show_retrieval = req.show_retrieval;

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
        source_filter,
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
                    .map(citation_response)
                    .collect(),
                verified: gen_result.verified,
            },
        ),
    )
    .await?;

    if let Some(debug) = retrieval_debug {
        send_stream_event(&tx, sse_json_event("retrieval", &debug)).await?;
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
    Ok(AskResponse {
        answer: String::new(),
        citations: Vec::new(),
        verified: false,
        retrieval: None,
        context: Some(context),
    })
}

fn context_only_retrieve_request(req: AskRequest) -> RetrieveRequest {
    RetrieveRequest {
        question: req.question,
        source_id: req.source_id,
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

struct RetrieveResponseInput {
    task_id: TaskId,
    query: String,
    source_filter: Option<SourceId>,
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
    let (result, profile_id) =
        run_indexing_operation(Arc::clone(&state), task_id, &controls, "ingest").await?;
    let response = IngestResponse { ingested: result };
    finish_task_success(&state, task_id, ingest_result_metadata(response.ingested)).await?;
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
    let (result, profile_id) =
        run_indexing_operation(Arc::clone(&state), task_id, &controls, "reindex").await?;
    let response = ReindexResponse { reindexed: result };
    finish_task_success(&state, task_id, reindex_result_metadata(response.reindexed)).await?;
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
) -> Result<(usize, EmbeddingProfileId), (StatusCode, Json<ErrorResponse>)> {
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
    let result = tokio::task::spawn_blocking(move || {
        let mut pipeline = state2.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        if vectors_only {
            let source_filter = source_id.as_ref().map(|id| SourceId(id.clone()));
            return runtime.block_on(
                pipeline.build_embedding_profile(&profile_id_for_task, source_filter.as_ref()),
            );
        }
        match source_id {
            Some(id) => {
                runtime.block_on(pipeline.ingest_source_with_task(&SourceId(id), &task_id2))?;
                Ok::<_, anyhow::Error>(1)
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
            result as u64,
            controls.source_id.as_ref().map(|_| 1_u64),
        )
        .with_recent_status(format!("{phase_name} complete"))
        .with_active_worker_kind(TaskKind::Ingest.as_str());
    let finished = timing.finish(serde_json::json!({
        "rebuilt": result,
        "force": controls.force,
        "embedding_profile_id": profile_id.as_str(),
        "vectors_only": controls.vectors_only,
        "source_id": controls.source_id.as_deref(),
    }));
    if controls.vectors_only {
        progress.set_endpoint(TaskEndpointSummary::single_call(
            "embedding",
            finished.duration_ms,
        ));
    }
    record_task_progress(&state, task_id, progress).await;
    record_task_span(&state, task_id, finished).await?;
    Ok((result, profile_id))
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
    source_filter: Option<SourceId>,
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
            .block_on(retrieval.search_filtered_with_debug(&question2, source_filter_ref))?;
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
    source_filter: Option<SourceId>,
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
                .block_on(retrieval.search_filtered_with_debug(&question2, source_filter_ref))?;
            (results, Some(debug))
        } else {
            let results =
                runtime.block_on(retrieval.search_filtered(&question2, source_filter_ref))?;
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
    let results_page = retrieve_result_page(
        &results,
        &debug,
        &source_paths,
        controls.limit,
        controls.page_size,
        controls.page,
        controls.include_locator,
    );
    let debug = controls.include_debug.then_some(debug);

    RetrieveResponse {
        task_id: task_id.0,
        query,
        source_id: source_filter.map(|source_id| source_id.0),
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

fn retrieve_result_page(
    results: &[RetrievalResult],
    debug: &RetrievalDebug,
    source_paths: &HashMap<String, String>,
    limit: usize,
    page_size: usize,
    page: usize,
    include_locator: bool,
) -> Vec<RetrieveResultResponse> {
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

fn citation_response(citation: CitationRef) -> CitationResponse {
    let kind = citation_kind_name(&citation);
    CitationResponse {
        label: citation.label,
        evidence_id: citation.evidence_id.0,
        kind: kind.to_string(),
        derived_from: citation.derived_from.map(|id| id.0),
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

    let mut runtime = state
        .runtime_config
        .write()
        .map_err(|error| anyhow::anyhow!("{error}"))?;
    runtime.config = next_config;
    runtime.reload = metadata.clone();
    Ok(metadata)
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
        runtime_config: std::sync::RwLock::new(RuntimeConfigState {
            config,
            reload: initial_reload_metadata(&config_path),
        }),
        config_path,
        data_dir,
    });
    let _config_watcher = start_config_watcher(Arc::clone(&state))?;
    schedule_ingest_queue(Arc::clone(&state));

    let app = Router::new()
        .route("/api/health", get(health))
        .route("/api/config", get(get_config))
        .route("/api/sources", post(add_source))
        .route("/api/sources", get(list_sources))
        .route("/api/sources/{id}", get(get_source))
        .route("/api/sources/{id}", delete(delete_source))
        .route("/api/sources/check", post(check_stale))
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
        Chunk, ChunkId, ChunkType, EvidenceKind, EvidenceUnit, RetrievalEvidenceRole,
        RetrievalProvenance, SourceLocator,
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
        assert_eq!(model_server.embedding_requests(), 2);
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

        finish_task_success(&state, &running_id, ingest_result_metadata(0))
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

        finish_task_success(&state, &background_id, ingest_result_metadata(0))
            .await
            .unwrap();
        ensure_ingest_task_started(&state, &foreground_id)
            .await
            .unwrap();

        assert_eq!(running_ingest_count(&state).await, 1);
        finish_task_success(&state, &foreground_id, ingest_result_metadata(0))
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

        finish_task_success(&state, &foreground_id, ingest_result_metadata(0))
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
