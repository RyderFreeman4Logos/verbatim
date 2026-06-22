use std::collections::HashSet;
use std::convert::Infallible;
use std::fs;
use std::io::Write;
use std::path::{Component, Path as FsPath, PathBuf};
use std::sync::Arc;

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
    HealthResponse, ImageArtifactResponse, IngestResponse, SourceResponse,
};
use verbatim_core::config::{self, Config};
use verbatim_core::embed::OpenAiEmbeddingClient;
use verbatim_core::generate::{
    image_artifact_evidence_id, select_image_attachments, GenerationContext, Generator,
};
use verbatim_core::graphrag::GraphRagService;
use verbatim_core::ingest::IngestPipeline;
use verbatim_core::provider::openai_compatible::OpenAiCompatibleReranker;
use verbatim_core::retrieve::{refresh_final_evidence_pack_debug, RetrievalPipeline};
use verbatim_core::store::Store;
use verbatim_core::types::{
    CitationRef, EvidenceId, EvidenceKind, ImageArtifact, RetrievalDebug, RetrievalResult, SourceId,
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

#[derive(Deserialize)]
struct IngestQuery {
    #[serde(default)]
    force: bool,
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

async fn ingest_all(
    State(state): State<SharedState>,
    query: Query<IngestQuery>,
) -> Result<Json<IngestResponse>, (StatusCode, Json<ErrorResponse>)> {
    let force = query.force;
    // Ingest is async (embeds, context gen) but also touches the !Send store.
    // We use a dedicated tokio runtime thread via `spawn_blocking` combined
    // with a local async runtime to drive the pipeline's async work.
    let state = Arc::clone(&state);
    let runtime = tokio::runtime::Handle::current();
    let result = tokio::task::spawn_blocking(move || {
        let mut pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        runtime.block_on(pipeline.ingest_all(force))
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(IngestResponse { ingested: result }))
}

async fn ingest_one(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<IngestResponse>, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(&state);
    let runtime = tokio::runtime::Handle::current();
    tokio::task::spawn_blocking(move || {
        let mut pipeline = state.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        runtime.block_on(pipeline.ingest_source(&SourceId(id)))
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(Json(IngestResponse { ingested: 1 }))
}

async fn ask(
    State(state): State<SharedState>,
    Json(req): Json<AskRequest>,
) -> Result<Json<AskResponse>, (StatusCode, Json<ErrorResponse>)> {
    execute_ask(state, req).await.map(Json)
}

async fn ask_stream(
    State(state): State<SharedState>,
    Json(req): Json<AskRequest>,
) -> Sse<impl Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = mpsc::channel::<Event>(ASK_STREAM_EVENT_BUFFER);
    tokio::spawn(async move {
        if let Err((status, Json(error))) = execute_ask_stream(state, req, tx.clone()).await {
            let _ = tx.send(sse_error_event(status, error.error)).await;
        }
    });

    Sse::new(stream::unfold(rx, |mut rx| async {
        rx.recv().await.map(|event| (Ok(event), rx))
    }))
}

async fn execute_ask(
    state: SharedState,
    req: AskRequest,
) -> Result<AskResponse, (StatusCode, Json<ErrorResponse>)> {
    let question = req.question;
    let source_filter = req.source_id.map(SourceId);
    let show_retrieval = req.show_retrieval;

    let (results, generation_context, retrieval_debug) =
        prepare_generation_context(Arc::clone(&state), &question, source_filter, show_retrieval)
            .await?;

    // Step 2: generate (Send-safe, no store access)
    let gen_result = state
        .generator
        .generate_with_context(&question, &results, &generation_context)
        .await
        .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

    Ok(AskResponse {
        answer: gen_result.answer,
        citations: gen_result
            .citations
            .into_iter()
            .map(citation_response)
            .collect(),
        verified: gen_result.verified,
        retrieval: retrieval_debug,
    })
}

async fn execute_ask_stream(
    state: SharedState,
    req: AskRequest,
    tx: mpsc::Sender<Event>,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let question = req.question;
    let source_filter = req.source_id.map(SourceId);
    let show_retrieval = req.show_retrieval;

    let (results, generation_context, retrieval_debug) =
        prepare_generation_context(Arc::clone(&state), &question, source_filter, show_retrieval)
            .await?;

    let tx_tokens = tx.clone();
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

    send_stream_event(
        &tx,
        sse_json_event(
            "citation",
            &AskCitationEvent {
                citations: gen_result
                    .citations
                    .into_iter()
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

    Ok(())
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

async fn prepare_generation_context(
    state: SharedState,
    question: &str,
    source_filter: Option<SourceId>,
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
    let runtime = tokio::runtime::Handle::current();
    let (results, generation_context, retrieval_debug) = tokio::task::spawn_blocking(move || {
        let pipeline = state2.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let lexical_index = pipeline.lexical_index();
        let mut retrieval = RetrievalPipeline::new_with_graph(
            pipeline.vector_index(),
            &lexical_index,
            pipeline.store(),
            &state2.embed_client,
            &state2.config.retrieval,
            &state2.config.graph,
        )
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
    let reranker = config
        .rerank
        .enabled
        .then(|| OpenAiCompatibleReranker::from_config(&config.rerank));

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

    #[tokio::test]
    async fn ask_stream_token_queue_is_bounded() {
        let (tx, _rx) = mpsc::channel::<Event>(1);

        try_send_stream_event(&tx, Event::default().event("token").data("one")).unwrap();
        let error =
            try_send_stream_event(&tx, Event::default().event("token").data("two")).unwrap_err();

        assert!(error.to_string().contains("not keeping up"));
    }
}
