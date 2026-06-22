use std::collections::{HashMap, HashSet};
use std::convert::Infallible;
use std::fs;
use std::io::Write;
use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use anyhow::{bail, Context, Result};
use axum::extract::{Path, Query, State};
use axum::http::StatusCode;
use axum::response::sse::{Event, Sse};
use axum::routing::{delete, get, post};
use axum::{Json, Router};
use futures::stream;
use futures::Stream;
use serde::{Deserialize, Serialize};
use tokio::net::TcpListener;
use tokio::signal;
use tokio::sync::mpsc;
use tower_http::cors::CorsLayer;

use verbatim_core::api::{
    AddSourceRequest, AddSourceResponse, AskCitationEvent, AskErrorEvent, AskRequest, AskResponse,
    AskTokenEvent, CheckStaleResponse, CitationResponse, ErrorResponse, EvidenceResponse,
    HealthResponse, ImageArtifactResponse, IngestResponse, RetrieveControlsResponse,
    RetrieveRequest, RetrieveResponse, RetrieveResultResponse, RetrieveTimingResponse,
    SourceResponse, TaskCreatedResponse, TaskEventsResponse, TaskIngestRequest,
    TaskSummaryResponse, TaskWaitEvent,
};
use verbatim_core::config::{self, Config, RerankConfig, RetrievalConfig};
use verbatim_core::embed::OpenAiEmbeddingClient;
use verbatim_core::generate::{
    image_artifact_evidence_id, select_image_attachments, GenerationContext, Generator,
};
use verbatim_core::graphrag::GraphRagService;
use verbatim_core::ingest::IngestPipeline;
use verbatim_core::provider::openai_compatible::OpenAiCompatibleReranker;
use verbatim_core::retrieve::{refresh_final_evidence_pack_debug, RetrievalPipeline};
use verbatim_core::store::Store;
use verbatim_core::task::{
    ask_request_metadata, ask_result_metadata, bounded_error, ingest_request_metadata,
    ingest_result_metadata, retrieve_request_metadata, retrieve_result_metadata, PhaseTiming,
    TaskId, TaskKind,
};
use verbatim_core::types::{
    CitationRef, EmbeddingProfileId, EvidenceId, EvidenceKind, ImageArtifact, RetrievalDebug,
    RetrievalEvidenceRole, RetrievalResult, SourceId,
};

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
    generator: Generator,
    embed_client: OpenAiEmbeddingClient,
    reranker: Option<OpenAiCompatibleReranker>,
    config: Config,
    data_dir: PathBuf,
}

type SharedState = Arc<AppState>;

const ASK_STREAM_EVENT_BUFFER: usize = 32;
const TASK_WAIT_EVENT_BUFFER: usize = 16;
const TASK_WAIT_EVENT_LIMIT: usize = 100;
const TASK_WAIT_POLL_INTERVAL: Duration = Duration::from_secs(1);
const FAST_RETRIEVAL_TOP_K: usize = 20;
const DEFAULT_SNIPPET_CHARS: usize = 240;

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

// ---------------------------------------------------------------------------
// Handlers
// ---------------------------------------------------------------------------

async fn health() -> Json<HealthResponse> {
    Json(HealthResponse {
        status: "ok".into(),
    })
}

async fn get_config(State(state): State<SharedState>) -> Json<serde_json::Value> {
    Json(state.config.redacted_json())
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
        pipeline.store().list_sources()
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(
        sources
            .into_iter()
            .map(|s| SourceResponse {
                id: s.id.0,
                path: s.path.to_string_lossy().into_owned(),
                status: format!("{:?}", s.status),
                hash: s.hash,
                parser_used: s.parser_used,
                last_ingested_at: s.last_ingested_at,
            })
            .collect(),
    ))
}

async fn get_source(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<SourceResponse>, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(&state);
    let id_clone = id.clone();
    let source = tokio::task::spawn_blocking(move || {
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        pipeline.store().get_source(&SourceId(id_clone))
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    match source {
        Some(s) => Ok(Json(SourceResponse {
            id: s.id.0,
            path: s.path.to_string_lossy().into_owned(),
            status: format!("{:?}", s.status),
            hash: s.hash,
            parser_used: s.parser_used,
            last_ingested_at: s.last_ingested_at,
        })),
        None => Err(err(
            StatusCode::NOT_FOUND,
            anyhow::anyhow!("source not found: {id}"),
        )),
    }
}

async fn delete_source(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(&state);
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let mut pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        runtime.block_on(pipeline.remove_source(&SourceId(id)))
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(StatusCode::NO_CONTENT)
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
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        pipeline.store().create_task(&task_id, kind, &request)?;
        pipeline.store().insert_task_event(
            &task_id,
            "queued",
            "task queued",
            &serde_json::json!({ "kind": kind.as_str() }),
        )?;
        Ok::<_, anyhow::Error>(task_id)
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))
}

async fn mark_task_started(
    state: &SharedState,
    task_id: &TaskId,
) -> Result<bool, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(state);
    let task_id = task_id.clone();
    tokio::task::spawn_blocking(move || {
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let started = pipeline.store().start_task(&task_id)?;
        if !started {
            return Ok(false);
        }
        pipeline.store().insert_task_event(
            &task_id,
            "started",
            "task started",
            &serde_json::json!({}),
        )?;
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
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        pipeline
            .store()
            .insert_task_event(&task_id, event_type, &message, &payload)?;
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
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        pipeline.store().insert_task_span(
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

async fn finish_task_success(
    state: &SharedState,
    task_id: &TaskId,
    result: serde_json::Value,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(state);
    let task_id = task_id.clone();
    tokio::task::spawn_blocking(move || {
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        if pipeline.store().finish_task_success(&task_id, &result)? {
            pipeline
                .store()
                .insert_task_event(&task_id, "succeeded", "task succeeded", &result)?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))
}

async fn finish_task_failed(
    state: &SharedState,
    task_id: &TaskId,
    error_message: &str,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(state);
    let task_id = task_id.clone();
    let error_message = bounded_error(error_message);
    tokio::task::spawn_blocking(move || {
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        if pipeline
            .store()
            .finish_task_failed(&task_id, &error_message)?
        {
            pipeline.store().insert_task_event(
                &task_id,
                "failed",
                "task failed",
                &serde_json::json!({ "error": error_message }),
            )?;
        }
        Ok::<_, anyhow::Error>(())
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))
}

async fn task_summary_response(
    state: &SharedState,
    task_id: TaskId,
) -> Result<TaskSummaryResponse, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || {
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let task = pipeline
            .store()
            .get_task(&task_id)?
            .with_context(|| format!("task not found: {}", task_id.0))?;
        let spans = pipeline.store().list_task_spans(&task_id)?;
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
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        pipeline
            .store()
            .get_task(&task_id)?
            .with_context(|| format!("task not found: {}", task_id.0))?;
        let events = pipeline.store().list_task_events(
            &task_id,
            after,
            limit.unwrap_or(TASK_WAIT_EVENT_LIMIT),
        )?;
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
        ingest_request_metadata(None, query.force),
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
        ingest_request_metadata(Some(&id), false),
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

async fn ask(
    State(state): State<SharedState>,
    Json(req): Json<AskRequest>,
) -> Result<Json<AskResponse>, (StatusCode, Json<ErrorResponse>)> {
    let task_id = create_persisted_task(
        &state,
        TaskKind::Ask,
        ask_request_metadata(&req.question, req.source_id.as_deref(), req.show_retrieval),
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
        ask_request_metadata(&req.question, req.source_id.as_deref(), req.show_retrieval),
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
        ask_request_metadata(&req.question, req.source_id.as_deref(), req.show_retrieval),
    )
    .await?;
    spawn_ask_task(state, task_id.clone(), req);
    Ok(Json(TaskCreatedResponse { task_id: task_id.0 }))
}

async fn retrieve(
    State(state): State<SharedState>,
    Json(req): Json<RetrieveRequest>,
) -> Result<Json<RetrieveResponse>, (StatusCode, Json<ErrorResponse>)> {
    let controls = resolve_retrieve_controls(&req, &state.config)
        .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let task_id = create_persisted_task(
        &state,
        TaskKind::Retrieve,
        retrieve_request_metadata(
            &req.question,
            req.source_id.as_deref(),
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
        ingest_request_metadata(req.source_id.as_deref(), req.force),
    )
    .await?;
    spawn_ingest_task(
        state,
        task_id.clone(),
        req.source_id,
        req.force,
        req.embedding_profile_id,
        req.vectors_only,
    );
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
        let _ = finish_task_failed(&state, &task_id, &error.error).await;
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
        let _ = finish_task_failed(&state, &task_id, &error.error).await;
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
        &state.config.embedding.profile_id,
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;

    let timing = PhaseTiming::start("retrieval");
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
    ensure_task_started(&state, task_id).await?;
    let question = req.question;
    let source_filter = req.source_id.map(SourceId);
    let embedding_profile_id = parse_embedding_profile_id(
        req.embedding_profile_id.as_deref(),
        &state.config.embedding.profile_id,
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let show_retrieval = req.show_retrieval;

    let timing = PhaseTiming::start("retrieval");
    let (results, generation_context, retrieval_debug) = prepare_generation_context(
        Arc::clone(&state),
        &question,
        source_filter,
        &embedding_profile_id,
        show_retrieval,
    )
    .await?;
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
    let gen_result = state
        .generator
        .generate_with_context(&question, &results, &generation_context)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let chat_timing = timing.finish(serde_json::json!({
        "citation_count": gen_result.citations.len(),
        "verified": gen_result.verified,
    }));
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
        let _ = finish_task_failed(&state, &task_id, &error.error).await;
    }
    result
}

async fn execute_ask_stream_task_inner(
    state: SharedState,
    task_id: &TaskId,
    req: AskRequest,
    tx: mpsc::Sender<Event>,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    ensure_task_started(&state, task_id).await?;
    let question = req.question;
    let source_filter = req.source_id.map(SourceId);
    let embedding_profile_id = parse_embedding_profile_id(
        req.embedding_profile_id.as_deref(),
        &state.config.embedding.profile_id,
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let show_retrieval = req.show_retrieval;

    let timing = PhaseTiming::start("retrieval");
    let (results, generation_context, retrieval_debug) = prepare_generation_context(
        Arc::clone(&state),
        &question,
        source_filter,
        &embedding_profile_id,
        show_retrieval,
    )
    .await?;
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
    let gen_result = state
        .generator
        .generate_streaming_with_context(&question, &results, &generation_context, move |delta| {
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
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let chat_timing = timing.finish(serde_json::json!({
        "citation_count": gen_result.citations.len(),
        "verified": gen_result.verified,
        "streaming": true,
    }));
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

fn spawn_ingest_task(
    state: SharedState,
    task_id: TaskId,
    source_id: Option<String>,
    force: bool,
    embedding_profile_id: Option<String>,
    vectors_only: bool,
) {
    tokio::spawn(async move {
        let _ = execute_ingest_task(
            state,
            task_id,
            source_id,
            force,
            embedding_profile_id,
            vectors_only,
        )
        .await;
    });
}

async fn execute_ingest_task(
    state: SharedState,
    task_id: TaskId,
    source_id: Option<String>,
    force: bool,
    embedding_profile_id: Option<String>,
    vectors_only: bool,
) -> Result<IngestResponse, (StatusCode, Json<ErrorResponse>)> {
    let result = execute_ingest_task_inner(
        Arc::clone(&state),
        &task_id,
        source_id,
        force,
        embedding_profile_id,
        vectors_only,
    )
    .await;
    if let Err((_, Json(error))) = &result {
        let _ = finish_task_failed(&state, &task_id, &error.error).await;
    }
    result
}

async fn execute_ingest_task_inner(
    state: SharedState,
    task_id: &TaskId,
    source_id: Option<String>,
    force: bool,
    embedding_profile_id: Option<String>,
    vectors_only: bool,
) -> Result<IngestResponse, (StatusCode, Json<ErrorResponse>)> {
    ensure_task_started(&state, task_id).await?;
    if embedding_profile_id.is_some() && !vectors_only {
        return Err(err(
            StatusCode::BAD_REQUEST,
            anyhow::anyhow!(
                "embedding_profile_id is supported for vectors-only builds; set [embedding].profile_id for parse ingest"
            ),
        ));
    }
    let profile_id = parse_embedding_profile_id(
        embedding_profile_id.as_deref(),
        &state.config.embedding.profile_id,
    )
    .map_err(|e| err(StatusCode::BAD_REQUEST, e))?;
    let state2 = Arc::clone(&state);
    let task_id2 = task_id.clone();
    let profile_id_for_task = profile_id.clone();
    let runtime = tokio::runtime::Handle::current();
    let timing = PhaseTiming::start("ingest");
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
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;
    let response = IngestResponse { ingested: result };
    record_task_span(
        &state,
        task_id,
        timing.finish(serde_json::json!({
            "ingested": response.ingested,
            "force": force,
            "embedding_profile_id": profile_id.as_str(),
            "vectors_only": vectors_only,
        })),
    )
    .await?;
    finish_task_success(&state, task_id, ingest_result_metadata(response.ingested)).await?;
    Ok(response)
}

async fn cancel_task_record(
    state: &SharedState,
    task_id: &TaskId,
) -> Result<bool, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(state);
    let task_id = task_id.clone();
    tokio::task::spawn_blocking(move || {
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        pipeline
            .store()
            .get_task(&task_id)?
            .with_context(|| format!("task not found: {}", task_id.0))?;
        let changed = pipeline.store().cancel_task(&task_id)?;
        if changed {
            pipeline.store().insert_task_event(
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
        let pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let task = pipeline
            .store()
            .get_task(&task_id)?
            .with_context(|| format!("task not found: {}", task_id.0))?;
        let events = pipeline.store().list_task_events(&task_id, after, limit)?;
        let spans = if task.status.is_terminal() {
            pipeline.store().list_task_spans(&task_id)?
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
        let mut retrieval = RetrievalPipeline::new_with_graph(
            pipeline.vector_index(),
            &lexical_index,
            pipeline.store(),
            &state2.embed_client,
            &controls.retrieval_config,
            &state2.config.graph,
        )
        .require_embedding_profile(&embedding_profile_id)
        .with_qdrant_search(&state2.config.qdrant);
        if let Some(reranker) = state2.reranker.as_ref() {
            retrieval = retrieval.with_reranker(&controls.rerank_config, reranker);
        }
        let source_filter_ref = source_filter.as_ref();
        let (mut results, mut debug) = runtime
            .block_on(retrieval.search_filtered_with_debug(&question2, source_filter_ref))?;
        if state2.config.graph.global_search.enabled && source_filter_ref.is_none() {
            let global_results =
                GraphRagService::new(pipeline.store(), &state2.config.graph.global_search)
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
    let runtime = tokio::runtime::Handle::current();
    let (results, generation_context, retrieval_debug) = tokio::task::spawn_blocking(move || {
        let mut pipeline = state2.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        pipeline.select_embedding_profile(&embedding_profile_id)?;
        let lexical_index = pipeline.lexical_index();
        let mut retrieval = RetrievalPipeline::new_with_graph(
            pipeline.vector_index(),
            &lexical_index,
            pipeline.store(),
            &state2.embed_client,
            &state2.config.retrieval,
            &state2.config.graph,
        )
        .require_embedding_profile(&embedding_profile_id)
        .with_qdrant_search(&state2.config.qdrant);
        if let Some(reranker) = state2.reranker.as_ref() {
            retrieval = retrieval.with_reranker(&state2.config.rerank, reranker);
        }
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
        if state2.config.graph.global_search.enabled && source_filter_ref.is_none() {
            let global_results =
                GraphRagService::new(pipeline.store(), &state2.config.graph.global_search)
                    .global_search_results(&question2, None)?;
            prepend_global_results(&mut results, global_results, &mut retrieval_debug);
        }
        let image_artifacts = collect_image_artifacts_for_results(&results, pipeline.store())?;
        let image_attachments = select_image_attachments(
            &results,
            &image_artifacts,
            &state2.config.chat.vision_attachments,
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
        EvidenceKind::Image => "image",
        EvidenceKind::Generated => "generated",
    }
}

fn citation_kind_name(citation: &CitationRef) -> &'static str {
    match citation.kind {
        EvidenceKind::Text => "original_text",
        EvidenceKind::Image => "image_artifact",
        EvidenceKind::Generated if citation.derived_from.is_some() => "image_caption_generated",
        EvidenceKind::Generated => "generated",
    }
}

fn retrieval_role_name(role: RetrievalEvidenceRole) -> &'static str {
    match role {
        RetrievalEvidenceRole::OriginalText => "original_text",
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
    (
        status,
        Json(ErrorResponse {
            error: format!("{e:#}"),
        }),
    )
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

    let config = Config::load().context("failed to load config")?;
    let data_dir = config::data_dir(&config);
    let pipeline = IngestPipeline::new(&config, &data_dir)?;
    let generator = Generator::new(&config.chat, &config.verifier);
    let embed_client = OpenAiEmbeddingClient::new(&config.embedding);
    let reranker = Some(OpenAiCompatibleReranker::from_config(&config.rerank));

    let bind_addr = config.daemon.bind.clone();

    let state: SharedState = Arc::new(AppState {
        pipeline: std::sync::Mutex::new(pipeline),
        generator,
        embed_client,
        reranker,
        config,
        data_dir,
    });

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
        .route("/api/ask", post(ask))
        .route("/api/ask/stream", post(ask_stream))
        .route("/api/retrieve", post(retrieve))
        .route("/api/tasks/ask", post(submit_ask_task))
        .route("/api/tasks/ingest", post(submit_ingest_task))
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
        Chunk, ChunkId, ChunkType, EvidenceUnit, RetrievalEvidenceRole, RetrievalProvenance,
        SourceLocator,
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
    fn no_args_continue_to_daemon_startup() {
        let mut stdout = Vec::new();

        let handled = write_version_if_requested(std::iter::empty::<&str>(), &mut stdout).unwrap();

        assert!(!handled);
        assert!(stdout.is_empty());
    }

    #[test]
    fn ask_request_defaults_retrieval_debug_off() {
        let req: AskRequest =
            serde_json::from_value(serde_json::json!({"question": "What is cited?"})).unwrap();

        assert_eq!(req.question, "What is cited?");
        assert!(req.source_id.is_none());
        assert!(!req.show_retrieval);
    }

    #[test]
    fn ask_response_omits_retrieval_when_debug_is_off() {
        let response = AskResponse {
            answer: "Answer [E1].".into(),
            citations: Vec::new(),
            verified: false,
            retrieval: None,
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
                bm25_hits: Vec::new(),
                dense_hits: Vec::new(),
                rrf_fused_hits: Vec::new(),
                graph_expanded_hits: Vec::new(),
                reranker: verbatim_core::types::RetrievalRerankDebug::disabled(),
                final_evidence_pack: Vec::new(),
            }),
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

        let state = Arc::new(AppState {
            pipeline: std::sync::Mutex::new(pipeline),
            generator: Generator::new(&config.chat, &config.verifier),
            embed_client: OpenAiEmbeddingClient::new(&config.embedding),
            reranker: Some(OpenAiCompatibleReranker::from_config(&config.rerank)),
            config,
            data_dir: test_dir.path().to_path_buf(),
        });
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
