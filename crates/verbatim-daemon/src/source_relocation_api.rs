//! Daemon boundary for explicit, identity-preserving source relocation.

use super::*;
use axum::extract::rejection::JsonRejection;
use verbatim_core::api::RelocateSourceRequest;
use verbatim_core::resource::ResourceQueueError;
use verbatim_core::store::{
    is_sqlite_busy_error, map_storage_error, source_relocation_error_kind,
    SourceRelocationErrorKind, SqliteWriteOperation,
};
use verbatim_core::types::Source;

pub(super) async fn relocate_source(
    State(state): State<SharedState>,
    request: Result<Json<RelocateSourceRequest>, JsonRejection>,
) -> Result<Json<SourceResponse>, (StatusCode, Json<ErrorResponse>)> {
    let Json(request) = request.map_err(relocation_json_rejection)?;
    let source_id = SourceId(request.source_id);
    let new_path = PathBuf::from(request.new_path);
    let source = with_exclusive_pipeline(&state, move |pipeline| {
        Ok(pipeline.relocate_source(&source_id, &new_path))
    })
    .await
    .map_err(pipeline_access_error)?
    .map_err(relocation_operation_error)?;

    Ok(Json(catalog_source_response(source).map_err(|error| {
        err(StatusCode::INTERNAL_SERVER_ERROR, error)
    })?))
}

fn relocation_json_rejection(rejection: JsonRejection) -> (StatusCode, Json<ErrorResponse>) {
    let status = if matches!(
        &rejection,
        JsonRejection::JsonDataError(_) | JsonRejection::JsonSyntaxError(_)
    ) {
        StatusCode::BAD_REQUEST
    } else {
        rejection.status()
    };
    err(status, anyhow::anyhow!(rejection.body_text()))
}

fn relocation_operation_error(error: anyhow::Error) -> (StatusCode, Json<ErrorResponse>) {
    let error = map_storage_error(SqliteWriteOperation::Ingest, error);
    match source_relocation_error_kind(&error) {
        Some(SourceRelocationErrorKind::NotFound) => err(StatusCode::NOT_FOUND, error),
        Some(SourceRelocationErrorKind::Validation) => err(StatusCode::BAD_REQUEST, error),
        None if error
            .chain()
            .any(|cause| cause.downcast_ref::<ResourceQueueError>().is_some()) =>
        {
            err(StatusCode::SERVICE_UNAVAILABLE, error)
        }
        None if is_sqlite_busy_error(&error) => err(StatusCode::SERVICE_UNAVAILABLE, error),
        None => sqlite_durability_ops::indexing_operation_error(
            None,
            SqliteWriteOperation::Ingest,
            error,
        ),
    }
}

fn catalog_source_response(source: Source) -> Result<SourceResponse> {
    SourceResponse::new(
        source.id.0,
        source.path.to_string_lossy().into_owned(),
        format!("{:?}", source.status),
        source.hash,
        source.parser_used,
        source.last_ingested_at,
        None,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use verbatim_core::store::SqliteDurabilityError;

    #[test]
    fn issue_332_inner_writer_queue_failures_are_service_unavailable() {
        for error in [
            ResourceQueueError::Full {
                name: "sqlite_writer".into(),
                kind: "sqlite_write".into(),
                queue_capacity: 1,
            },
            ResourceQueueError::Timeout {
                name: "sqlite_writer".into(),
                kind: "sqlite_write".into(),
                timeout: Duration::from_millis(1),
            },
        ] {
            let (status, _) = relocation_operation_error(error.into());
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        }
    }

    #[test]
    fn issue_332_unknown_relocation_runtime_failure_is_internal() {
        let (status, _) = relocation_operation_error(anyhow::anyhow!(
            "statvfs failed without a durability classification"
        ));

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn issue_332_untyped_not_found_text_collision_is_internal() {
        let (status, _) = relocation_operation_error(anyhow::anyhow!("source not found: source-1"));

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn issue_332_native_sqlite_busy_and_locked_are_service_unavailable() {
        for code in [rusqlite::ffi::SQLITE_BUSY, rusqlite::ffi::SQLITE_LOCKED] {
            let error = rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None);
            let (status, _) = relocation_operation_error(error.into());
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        }
    }

    #[test]
    fn issue_332_disk_full_relocation_uses_insufficient_storage() {
        for error in [
            anyhow::Error::from(SqliteDurabilityError::DiskFull {
                operation: SqliteWriteOperation::Ingest,
            }),
            std::io::Error::from_raw_os_error(libc::ENOSPC).into(),
        ] {
            let (status, _) = relocation_operation_error(error);
            assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
        }
    }
}
