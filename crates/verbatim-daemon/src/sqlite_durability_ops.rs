//! SQLite durability orchestration at the daemon boundary.
//!
//! Health retrieval, write-capacity preflight, storage-error HTTP mapping,
//! and graceful shutdown WAL checkpointing are cohesive responsibilities of
//! the explicit durability contract selected for the task store. Keeping them
//! here leaves the daemon request handlers focused on routing and timing.

use super::*;
use verbatim_core::store::{map_storage_error, SqliteDurabilityError, SqliteWriteOperation};

/// Open the task store with the durability profile selected by configuration.
pub(super) fn open_task_store(config: &Config, data_dir: &FsPath) -> Result<Store> {
    Store::new_with_durability_profile(&data_dir.join("verbatim.db"), config.store.durability)
}

/// Effective durability state for the `/health` endpoint, or `None` when the
/// task-store lock is unavailable or SQLite cannot report status.
pub(super) fn health_durability_status(
    state: &SharedState,
) -> Option<verbatim_core::store::SqliteDurabilityStatus> {
    state
        .task_store
        .try_lock()
        .ok()
        .and_then(|store| store.durability_status().ok())
}

/// Classify the write-heavy operation and fail before it can consume the
/// SQLite filesystem reserve. Returns the operation for later error mapping.
pub(super) async fn preflight_indexing_capacity(
    state: &SharedState,
    controls: &IndexingTaskControls,
) -> Result<SqliteWriteOperation, (StatusCode, Json<ErrorResponse>)> {
    let write_operation = indexing_write_operation(controls);
    let state = Arc::clone(state);
    tokio::task::spawn_blocking(move || ensure_indexing_write_capacity(&state, write_operation))
        .await
        .map_err(|error| {
            err(
                StatusCode::INTERNAL_SERVER_ERROR,
                anyhow::anyhow!("join SQLite task-store capacity preflight: {error}"),
            )
        })??;
    Ok(write_operation)
}

/// Classify the write-heavy operation an indexing request performs so the
/// capacity preflight and storage-error mapper share one source of truth.
pub(super) fn indexing_write_operation(controls: &IndexingTaskControls) -> SqliteWriteOperation {
    if controls.vectors_only {
        SqliteWriteOperation::IndexBuild
    } else {
        SqliteWriteOperation::Ingest
    }
}

/// Fail before an indexing operation can consume the SQLite filesystem reserve.
pub(super) fn ensure_indexing_write_capacity(
    state: &SharedState,
    write_operation: SqliteWriteOperation,
) -> Result<(), (StatusCode, Json<ErrorResponse>)> {
    let store = state.task_store.lock().map_err(|error| {
        err(
            StatusCode::INTERNAL_SERVER_ERROR,
            anyhow::anyhow!("lock SQLite task store for capacity preflight: {error}"),
        )
    })?;
    store
        .ensure_write_capacity(write_operation)
        .map_err(|error| {
            tracing::error!(operation = write_operation.as_str(), error = %error, "SQLite disk reserve rejected indexing operation");
            err(StatusCode::INSUFFICIENT_STORAGE, error)
        })?;
    Ok(())
}

/// Run the profile-scheduled WAL checkpoint after a successful indexing
/// operation, threading the outcome through so callers keep their pipeline
/// result binding.
pub(super) fn checkpoint_after_index<T>(
    pipeline: &IngestPipeline,
    label: &str,
    outcome: T,
) -> Result<T> {
    pipeline
        .checkpoint_wal()
        .with_context(|| format!("checkpoint SQLite WAL after {label}"))?;
    Ok(outcome)
}

/// Map a SQLite storage failure from an operation to the matching HTTP
/// status, preserving source-not-found and generic internal-error fallbacks.
pub(super) fn indexing_operation_error(
    source_id: Option<&str>,
    write_operation: SqliteWriteOperation,
    error: anyhow::Error,
) -> (StatusCode, Json<ErrorResponse>) {
    let error = map_storage_error(write_operation, error);
    let durability_error = error
        .chain()
        .find_map(|cause| cause.downcast_ref::<SqliteDurabilityError>());
    let status = match (source_id, durability_error) {
        (
            _,
            Some(
                SqliteDurabilityError::DiskReserve { .. } | SqliteDurabilityError::DiskFull { .. },
            ),
        ) => StatusCode::INSUFFICIENT_STORAGE,
        (_, Some(SqliteDurabilityError::WalCheckpointBlocked { .. })) => {
            StatusCode::SERVICE_UNAVAILABLE
        }
        (Some(source_id), _) if is_source_not_found_error(source_id, &error) => {
            StatusCode::NOT_FOUND
        }
        _ => StatusCode::INTERNAL_SERVER_ERROR,
    };
    err(status, error)
}

/// Execute the indexing pipeline on a blocking thread, checkpointing the WAL
/// after a successful build or ingest. Returns the outcome and the cached
/// index status snapshot captured before the pipeline was released.
pub(super) async fn run_indexing_with_pipeline(
    state: SharedState,
    vectors_only: bool,
    source_id: Option<String>,
    profile_id: EmbeddingProfileId,
    task_id: TaskId,
    force: bool,
) -> Result<(Result<IndexingOutcome>, Option<IndexStatusResponse>), (StatusCode, Json<ErrorResponse>)>
{
    let runtime = tokio::runtime::Handle::current();
    let state_for_pipeline = Arc::clone(&state);
    tokio::task::spawn_blocking(move || {
        run_with_pipeline(state_for_pipeline, move |pipeline| {
            if vectors_only {
                let source_filter = source_id.as_ref().map(|id| SourceId(id.clone()));
                let result = runtime.block_on(
                    pipeline.build_embedding_profile(&profile_id, source_filter.as_ref()),
                );
                let result = result
                    .and_then(|outcome| checkpoint_after_index(pipeline, "index build", outcome));
                let index_status = initial_index_status_cache(pipeline);
                return Ok((result, index_status));
            }
            let result = match source_id {
                Some(id) => {
                    match runtime
                        .block_on(pipeline.ingest_source_with_task(&SourceId(id), &task_id))
                    {
                        Ok(embedding_cache) => Ok(IndexingOutcome {
                            source_count: 1,
                            skipped_missing_sources: 0,
                            embedding_cache,
                        }),
                        Err(error) => Err(error),
                    }
                }
                None => runtime.block_on(pipeline.ingest_all_with_task(force, &task_id)),
            };
            let result =
                result.and_then(|outcome| checkpoint_after_index(pipeline, "ingest", outcome));
            let index_status = initial_index_status_cache(pipeline);
            Ok((result, index_status))
        })
    })
    .await
    .map_err(|e| err(StatusCode::INTERNAL_SERVER_ERROR, anyhow::anyhow!(e)))
    .and_then(|inner| inner.map_err(pipeline_access_error))
}

/// Run the graceful-shutdown WAL checkpoint. A failed checkpoint is logged so
/// the next open can run recovery checks; a poisoned lock is non-fatal.
pub(super) fn shutdown_checkpoint(state: &SharedState) {
    match state.task_store.lock() {
        Ok(store) => {
            if let Err(error) = store.checkpoint_wal_on_shutdown() {
                tracing::warn!(error = %error, "SQLite shutdown checkpoint failed; recovery checks will run on next open");
            }
        }
        Err(error) => {
            tracing::warn!(%error, "SQLite shutdown checkpoint skipped because task store lock is poisoned")
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::tests::retrieve_test_config;
    use std::sync::{mpsc, Arc};
    use std::time::{Duration, Instant};

    struct TestDir(std::path::PathBuf);

    impl Drop for TestDir {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    fn test_state(name: &str) -> (TestDir, SharedState) {
        let unique = format!(
            "verbatim-daemon-{name}-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        );
        let data_dir = TestDir(std::env::temp_dir().join(unique));
        std::fs::create_dir_all(&data_dir.0).unwrap();
        let config = retrieve_test_config("http://127.0.0.1:9/v1");
        let pipeline = IngestPipeline::new(&config, &data_dir.0).unwrap();
        let state = crate::tests::test_state(config, &data_dir.0, pipeline);
        (data_dir, state)
    }

    #[test]
    fn health_durability_status_returns_none_without_waiting_for_task_store_lock() {
        let (_test_dir, state) = test_state("health-durability-try-lock");
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let holder_state = Arc::clone(&state);
        let holder = std::thread::spawn(move || {
            let _store = holder_state.task_store.lock().unwrap();
            locked_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_millis(250));
        });
        locked_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let started = Instant::now();
        let status = health_durability_status(&state);

        assert!(
            started.elapsed() < Duration::from_millis(100),
            "health durability status waited for a held task-store lock"
        );
        assert!(status.is_none());
        release_tx.send(()).unwrap();
        holder.join().unwrap();
    }

    #[tokio::test(flavor = "current_thread")]
    async fn indexing_capacity_preflight_does_not_block_the_runtime_worker() {
        let (_test_dir, state) = test_state("indexing-capacity-blocking-worker");
        let controls = IndexingTaskControls {
            source_id: None,
            force: false,
            embedding_profile_id: None,
            vectors_only: false,
            ingest_batch_id: None,
        };
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let holder_state = Arc::clone(&state);
        let holder = std::thread::spawn(move || {
            let _store = holder_state.task_store.lock().unwrap();
            locked_tx.send(()).unwrap();
            let _ = release_rx.recv_timeout(Duration::from_millis(500));
        });
        locked_rx.recv_timeout(Duration::from_secs(1)).unwrap();

        let started = Instant::now();
        let (entered_tx, entered_rx) = tokio::sync::oneshot::channel();
        let preflight_state = Arc::clone(&state);
        let preflight = tokio::spawn(async move {
            entered_tx.send(()).unwrap();
            preflight_indexing_capacity(&preflight_state, &controls).await
        });
        entered_rx.await.unwrap();

        assert!(
            started.elapsed() < Duration::from_millis(200),
            "capacity preflight blocked the current-thread Tokio runtime"
        );
        release_tx.send(()).unwrap();
        assert_eq!(
            preflight.await.unwrap().unwrap(),
            SqliteWriteOperation::Ingest
        );
        holder.join().unwrap();
    }
}
