//! Daemon boundary for explicit, identity-preserving source relocation.

use super::*;
use verbatim_core::api::RelocateSourceRequest;
use verbatim_core::store::{is_sqlite_storage_error, SqliteWriteOperation};
use verbatim_core::types::Source;

pub(super) async fn relocate_source(
    State(state): State<SharedState>,
    Path(id): Path<String>,
    Json(request): Json<RelocateSourceRequest>,
) -> Result<Json<SourceResponse>, (StatusCode, Json<ErrorResponse>)> {
    let source_id = SourceId(id.clone());
    let new_path = PathBuf::from(request.new_path);
    let source = with_exclusive_pipeline(&state, move |pipeline| {
        Ok(pipeline.relocate_source(&source_id, &new_path))
    })
    .await
    .map_err(pipeline_access_error)?
    .map_err(|error| relocation_operation_error(&id, error))?;

    Ok(Json(catalog_source_response(source)))
}

fn relocation_operation_error(
    source_id: &str,
    error: anyhow::Error,
) -> (StatusCode, Json<ErrorResponse>) {
    if is_source_not_found_error(source_id, &error) {
        err(StatusCode::NOT_FOUND, error)
    } else if is_sqlite_storage_error(&error) {
        sqlite_durability_ops::indexing_operation_error(
            Some(source_id),
            SqliteWriteOperation::Ingest,
            error,
        )
    } else {
        err(StatusCode::BAD_REQUEST, error)
    }
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

#[cfg(test)]
mod tests {
    use super::*;
    use verbatim_core::store::SqliteDurabilityError;

    #[test]
    fn issue_332_disk_full_relocation_uses_insufficient_storage() {
        let error = SqliteDurabilityError::DiskFull {
            operation: SqliteWriteOperation::Ingest,
        }
        .into();

        let (status, _) = relocation_operation_error("source-1", error);

        assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
    }
}
