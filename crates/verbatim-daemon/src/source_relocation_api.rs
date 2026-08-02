//! Daemon boundary for explicit, identity-preserving source relocation.

use super::*;
use verbatim_core::api::RelocateSourceRequest;
use verbatim_core::types::Source;

pub(super) async fn relocate_source(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Json(request): Json<RelocateSourceRequest>,
) -> Result<Json<SourceResponse>, (StatusCode, Json<ErrorResponse>)> {
    let source_id = SourceId(id.clone());
    let new_path = PathBuf::from(request.new_path);
    let source = with_exclusive_pipeline(&state, move |pipeline| {
        pipeline.relocate_source(&source_id, &new_path)
    })
    .await
    .map_err(|error| {
        if is_pipeline_busy_error(&error) {
            pipeline_access_error(error)
        } else if is_source_not_found_error(&id, &error) {
            err(StatusCode::NOT_FOUND, error)
        } else {
            err(StatusCode::BAD_REQUEST, error)
        }
    })?;

    Ok(Json(catalog_source_response(source)))
}

fn catalog_source_response(source: Source) -> SourceResponse {
    SourceResponse {
        id: source.id.0,
        path: source.path.to_string_lossy().into_owned(),
        status: format!("{:?}", source.status),
        hash: source.hash,
        parser_used: source.parser_used,
        last_ingested_at: source.last_ingested_at,
        diagnostics: None,
    }
}
