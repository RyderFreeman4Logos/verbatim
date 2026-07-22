//! Daemon-bound erasure handlers and startup reconciliation.

use super::*;
use verbatim_core::deletion::PersistedDeletionReport;

pub(super) const STARTUP_DELETION_RECONCILE_BATCH_SIZE: usize = 16;

pub(super) async fn delete_source(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<StatusCode, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(&state);
    let runtime = tokio::runtime::Handle::current();
    let source_id = SourceId(id.clone());
    tokio::task::spawn_blocking(move || {
        run_with_pipeline(state, move |pipeline| {
            runtime.block_on(pipeline.remove_source(&source_id))
        })
    })
    .await
    .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error.into()))?
    .map_err(|error| {
        if is_pipeline_busy_error(&error) {
            pipeline_access_error(error)
        } else {
            source_remove_error(&id, error)
        }
    })?;

    Ok(StatusCode::NO_CONTENT)
}

/// Return durable, content-free deletion receipts for operational audit.
pub(super) async fn list_deletion_reports(
    State(state): State<SharedState>,
) -> Result<Json<Vec<PersistedDeletionReport>>, (StatusCode, Json<ErrorResponse>)> {
    let reports = with_task_store_read(&state, |store| store.list_deletion_reports())
        .await
        .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?;
    Ok(Json(reports))
}

/// Reconcile pending remote erasures after the pipeline has been restored at startup.
pub(super) async fn reconcile_deletions_on_startup(
    state: &SharedState,
    max_sources: usize,
) -> Result<()> {
    let state = Arc::clone(state);
    let runtime = tokio::runtime::Handle::current();
    let reports = tokio::task::spawn_blocking(move || {
        run_with_pipeline(state, move |pipeline| {
            runtime.block_on(pipeline.reconcile_deletions_up_to(max_sources))
        })
    })
    .await
    .context("join startup deletion reconciliation")??;
    if !reports.is_empty() {
        tracing::info!(
            count = reports.len(),
            max_sources,
            "reconciled pending source deletions at startup"
        );
    }
    Ok(())
}
