//! Dedicated lookup for reserved GraphRAG report-artifact ids.

use super::*;
use verbatim_core::graphrag::ReportArtifactManifest;
use verbatim_core::types::report_artifact::ReportArtifactId;

pub(super) async fn get_report_artifact(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Json<ReportArtifactManifest>, (StatusCode, Json<ErrorResponse>)> {
    let artifact_id = ReportArtifactId::parse(&id).map_err(|error| {
        err(
            StatusCode::BAD_REQUEST,
            anyhow::anyhow!("invalid report artifact id: {error}"),
        )
    })?;
    let graph_config = runtime_config_snapshot(&state)
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?
        .config
        .graph
        .global_search;
    let resolved = with_exclusive_pipeline(&state, move |pipeline| {
        GraphRagService::new(pipeline.store(), &graph_config).resolve_report_artifact(&artifact_id)
    })
    .await
    .map_err(pipeline_access_error)?;
    match resolved {
        Some(manifest) => Ok(Json(manifest)),
        None => {
            let mut response = ErrorResponse::new(format!("report artifact not found: {id}"));
            response.code = Some("report_artifact_not_found".into());
            Err((StatusCode::NOT_FOUND, Json(response)))
        }
    }
}
