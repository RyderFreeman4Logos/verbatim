//! Daemon boundary for explicit, identity-preserving source relocation.

use super::*;
use verbatim_core::api::RelocateSourceRequest;
use verbatim_core::resource::ResourceQueueError;
use verbatim_core::store::{
    map_storage_error, source_relocation_error_kind, SourceRelocationErrorKind,
    SqliteWriteOperation,
};
use verbatim_core::types::Source;

pub(super) async fn relocate_source(
    State(state): State<SharedState>,
    Path(segment): Path<String>,
    Json(request): Json<RelocateSourceRequest>,
) -> Result<Json<SourceResponse>, (StatusCode, Json<ErrorResponse>)> {
    let id = decode_source_id_path_segment(&segment)
        .map_err(|error| err(StatusCode::BAD_REQUEST, error))?;
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
        None => sqlite_durability_ops::indexing_operation_error(
            Some(source_id),
            SqliteWriteOperation::Ingest,
            error,
        ),
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
    fn issue_332_source_id_path_decoder_strips_exactly_one_prefix() {
        for (segment, expected) in [
            ("~.", "."),
            ("~..", ".."),
            ("~%", "%"),
            ("~雪", "雪"),
            ("~/", "/"),
            ("~?", "?"),
            ("~#", "#"),
            ("~~prefixed", "~prefixed"),
        ] {
            assert_eq!(decode_source_id_path_segment(segment).unwrap(), expected);
        }
        assert!(decode_source_id_path_segment("unframed").is_err());
    }

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
            let (status, _) = relocation_operation_error("source-1", error.into());
            assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
        }
    }

    #[test]
    fn issue_332_unknown_relocation_runtime_failure_is_internal() {
        let (status, _) = relocation_operation_error(
            "source-1",
            anyhow::anyhow!("statvfs failed without a durability classification"),
        );

        assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
    }

    #[test]
    fn issue_332_disk_full_relocation_uses_insufficient_storage() {
        for error in [
            anyhow::Error::from(SqliteDurabilityError::DiskFull {
                operation: SqliteWriteOperation::Ingest,
            }),
            std::io::Error::from_raw_os_error(libc::ENOSPC).into(),
        ] {
            let (status, _) = relocation_operation_error("source-1", error);
            assert_eq!(status, StatusCode::INSUFFICIENT_STORAGE);
        }
    }
}
