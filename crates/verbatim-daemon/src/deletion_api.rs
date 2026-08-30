//! Daemon-bound erasure handlers and startup reconciliation.

use super::*;
use verbatim_core::deletion::{DeletionOutcome, DeletionProduct, PersistedDeletionReport};
use verbatim_core::DeletionReportResponse;

pub(super) const STARTUP_DELETION_RECONCILE_BATCH_SIZE: usize = 16;
pub(super) const DELETION_RECONCILE_INTERVAL: Duration = if cfg!(test) {
    Duration::from_millis(10)
} else {
    Duration::from_secs(30)
};

pub(super) async fn delete_source(
    State(state): State<SharedState>,
    Path(id): Path<String>,
) -> Result<Response, (StatusCode, Json<ErrorResponse>)> {
    let state = Arc::clone(&state);
    let runtime = tokio::runtime::Handle::current();
    let source_id = SourceId(id.clone());
    let state = Arc::clone(&state);
    let receipt = tokio::task::spawn_blocking(move || {
        run_with_pipeline(state, move |pipeline| {
            runtime.block_on(pipeline.remove_source(&source_id))?;
            pipeline
                .store()
                .latest_deletion_report(&source_id)?
                .ok_or_else(|| anyhow::anyhow!("missing deletion receipt for {}", source_id.0))
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

    // HNSW's pending status is an audit marker, not a scheduler-owned operation.
    // Only remote, image-artifact, and retained-backup work should make deletion async.
    let has_pending_work = [
        DeletionProduct::Qdrant,
        DeletionProduct::Images,
        DeletionProduct::Backups,
    ]
    .into_iter()
    .any(|product| receipt.report.status_for(product) == Some(DeletionOutcome::Pending));
    if has_pending_work {
        let response = DeletionReportResponse::new(receipt)
            .map_err(|error| err(StatusCode::INTERNAL_SERVER_ERROR, error))?;
        Ok((StatusCode::ACCEPTED, Json(response)).into_response())
    } else {
        Ok(StatusCode::NO_CONTENT.into_response())
    }
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

/// Reconcile one bounded batch of pending erasures after the pipeline is restored.
async fn reconcile_deletions_batch(state: &SharedState, max_sources: usize) -> Result<usize> {
    let state = Arc::clone(state);
    let runtime = tokio::runtime::Handle::current();
    let reports = tokio::task::spawn_blocking(move || {
        run_with_pipeline(state, move |pipeline| {
            runtime.block_on(pipeline.reconcile_deletions_up_to(max_sources))
        })
    })
    .await
    .context("join deletion reconciliation")??;
    if !reports.is_empty() {
        tracing::info!(
            count = reports.len(),
            max_sources,
            "reconciled pending source deletions"
        );
    }
    Ok(reports.len())
}

/// Reconcile the first bounded deletion batch before reporting the daemon ready.
pub(super) async fn reconcile_deletions_on_startup(
    state: &SharedState,
    max_sources: usize,
) -> Result<()> {
    reconcile_deletions_batch(state, max_sources).await?;
    Ok(())
}

/// Continue bounded deletion reconciliation after the daemon becomes ready.
pub(super) fn start_deletion_reconcile_scheduler(
    state: SharedState,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut interval = tokio::time::interval(DELETION_RECONCILE_INTERVAL);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            interval.tick().await;
            if let Err(error) =
                reconcile_deletions_batch(&state, STARTUP_DELETION_RECONCILE_BATCH_SIZE).await
            {
                tracing::warn!(error = %error, "background deletion reconciliation failed");
            }
        }
    })
}
