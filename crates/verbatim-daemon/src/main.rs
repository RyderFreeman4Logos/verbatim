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
use tower_http::cors::CorsLayer;

use verbatim_core::config::{self, Config};
use verbatim_core::embed::OpenAiEmbeddingClient;
use verbatim_core::generate::{
    image_artifact_evidence_id, select_image_attachments, GenerationContext, Generator,
};
use verbatim_core::ingest::IngestPipeline;
use verbatim_core::retrieve::RetrievalPipeline;
use verbatim_core::store::Store;
use verbatim_core::types::{
    BBox, CitationRef, EvidenceId, EvidenceKind, ImageArtifact, RetrievalResult, SourceId,
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
    config: Config,
    data_dir: PathBuf,
}

type SharedState = Arc<AppState>;

// ---------------------------------------------------------------------------
// Request / response types
// ---------------------------------------------------------------------------

#[derive(Deserialize)]
struct AddSourceRequest {
    path: String,
}

#[derive(Serialize)]
struct AddSourceResponse {
    id: String,
}

#[derive(Serialize)]
struct SourceResponse {
    id: String,
    path: String,
    status: String,
    hash: String,
    parser_used: Option<String>,
    last_ingested_at: Option<String>,
}

#[derive(Serialize)]
struct CheckStaleResponse {
    stale: Vec<String>,
}

#[derive(Deserialize)]
struct IngestQuery {
    #[serde(default)]
    force: bool,
}

#[derive(Serialize)]
struct IngestResponse {
    ingested: usize,
}

#[derive(Deserialize)]
struct AskRequest {
    question: String,
    #[serde(default)]
    source_id: Option<String>,
}

#[derive(Serialize)]
struct AskResponse {
    answer: String,
    citations: Vec<CitationResponse>,
    verified: bool,
}

#[derive(Serialize)]
struct CitationResponse {
    label: String,
    evidence_id: String,
    kind: &'static str,
    derived_from: Option<String>,
    locator: String,
    text_preview: String,
}

#[derive(Serialize)]
struct EvidenceResponse {
    id: String,
    source_id: String,
    kind: &'static str,
    derived_from: Option<String>,
    locator: String,
    text: String,
    heading_path: Vec<String>,
    position: u32,
    image_artifact: Option<ImageArtifactResponse>,
}

#[derive(Serialize)]
struct ImageArtifactResponse {
    image_id: String,
    path: String,
    content_hash: String,
    mime_type: String,
    width: u32,
    height: u32,
    page: u32,
    image_index: u32,
    bbox: Option<BBox>,
}

#[derive(Serialize)]
struct HealthResponse {
    status: String,
}

#[derive(Serialize)]
struct ErrorResponse {
    error: String,
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
    let event = match execute_ask(state, req).await {
        Ok(response) => Event::default()
            .event("answer")
            .json_data(response)
            .unwrap_or_else(|_| Event::default().event("error").data("serialize response")),
        Err((status, Json(error))) => Event::default().event("error").data(
            serde_json::json!({
                "status": status.as_u16(),
                "error": error.error,
            })
            .to_string(),
        ),
    };

    Sse::new(stream::once(async move { Ok(event) }))
}

async fn execute_ask(
    state: SharedState,
    req: AskRequest,
) -> Result<AskResponse, (StatusCode, Json<ErrorResponse>)> {
    let question = req.question;
    let source_filter = req.source_id.map(SourceId);

    // Step 1: retrieve (involves !Send store + async embed)
    let state2 = Arc::clone(&state);
    let question2 = question.clone();
    let runtime = tokio::runtime::Handle::current();
    let (results, generation_context) = tokio::task::spawn_blocking(move || {
        let pipeline = state2.pipeline.lock().map_err(|e| anyhow::anyhow!("{e}"))?;
        let lexical_index = pipeline.lexical_index();
        let retrieval = RetrievalPipeline::new_with_graph(
            pipeline.vector_index(),
            &lexical_index,
            pipeline.store(),
            &state2.embed_client,
            &state2.config.retrieval,
            &state2.config.graph,
        );
        let results =
            runtime.block_on(retrieval.search_filtered(&question2, source_filter.as_ref()))?;
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
        ))
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e.into()))?
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, e))?;

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
            .map(|c| {
                let kind = citation_kind_name(&c);
                CitationResponse {
                    label: c.label,
                    evidence_id: c.evidence_id.0,
                    kind,
                    derived_from: c.derived_from.map(|id| id.0),
                    locator: c.locator.to_string(),
                    text_preview: c.text_preview,
                }
            })
            .collect(),
        verified: gen_result.verified,
    })
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
            kind: evidence_kind_name(eu.kind),
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

    let bind_addr = config.daemon.bind.clone();

    let state: SharedState = Arc::new(AppState {
        pipeline: std::sync::Mutex::new(pipeline),
        generator,
        embed_client,
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
}
