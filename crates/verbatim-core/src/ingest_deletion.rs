use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

#[cfg(feature = "qdrant")]
use super::qdrant_mutation_fence;
use super::{
    acquire_ingest_resource, remove_source_image_artifacts, remove_staged_index_artifacts,
    source_path_is_missing, EmbeddingClient, IngestPipeline,
};
use crate::deletion::{DeletionOutcome, DeletionProduct, DeletionReport, RetentionPolicy};
use crate::task::{IngestTaskStage, TaskId, TaskProgressSnapshot};
use crate::types::SourceId;

#[derive(Clone, Copy)]
enum SourceRemovalKind {
    Erasure(RetentionPolicy),
    Housekeeping,
}

impl<E> IngestPipeline<E>
where
    E: EmbeddingClient,
{
    pub async fn remove_source(&mut self, source_id: &SourceId) -> Result<DeletionReport> {
        self.remove_source_with_retention(source_id, RetentionPolicy::Immediate)
            .await
    }

    pub async fn remove_source_with_retention(
        &mut self,
        source_id: &SourceId,
        retention_policy: RetentionPolicy,
    ) -> Result<DeletionReport> {
        let images = self
            .remove_source_locally(source_id, SourceRemovalKind::Erasure(retention_policy))
            .await?;

        #[cfg(feature = "qdrant")]
        let qdrant_outcome = self.sync_qdrant_delete_source(source_id).await;
        #[cfg(not(feature = "qdrant"))]
        let qdrant_outcome = DeletionOutcome::Pending;
        let mut report = self.local_deletion_report(source_id, images)?;
        report.set(DeletionProduct::Qdrant, qdrant_outcome);
        // Finalize after the remote await so we re-read retention under the same
        // single-writer bulkhead used by other durable SQLite writers.
        let _sqlite_write_permit = acquire_ingest_resource("sqlite_writer", "sqlite_write").await?;
        let mut transaction = self.store.connection().unchecked_transaction()?;
        self.store.finalize_deletion_outcome_tx(
            &mut transaction,
            source_id,
            qdrant_outcome,
            &mut report,
        )?;
        transaction.commit()?;
        Ok(report)
    }

    /// Remove a missing filesystem or collection member without creating an erasure tombstone.
    pub async fn remove_source_for_housekeeping(&mut self, source_id: &SourceId) -> Result<()> {
        self.remove_source_locally(source_id, SourceRemovalKind::Housekeeping)
            .await?;
        Ok(())
    }

    /// Reconcile remote deletion and retained-backup outcomes for deleted sources.
    pub async fn reconcile_deletions(&self) -> Result<Vec<DeletionReport>> {
        let source_ids = self.store.reconciliation_deletion_source_ids()?;
        self.reconcile_deletion_source_ids(source_ids).await
    }

    /// Reconcile no more than `max_sources` deletion candidates in one batch.
    pub async fn reconcile_deletions_up_to(
        &self,
        max_sources: usize,
    ) -> Result<Vec<DeletionReport>> {
        let source_ids = self
            .store
            .reconciliation_deletion_source_ids_up_to(max_sources)?;
        self.reconcile_deletion_source_ids(source_ids).await
    }

    async fn reconcile_deletion_source_ids(
        &self,
        source_ids: Vec<SourceId>,
    ) -> Result<Vec<DeletionReport>> {
        let mut reports = Vec::with_capacity(source_ids.len());
        for source_id in source_ids {
            let qdrant_outcome = match self
                .store
                .qdrant_deletion_outcome(&source_id)?
                .with_context(|| format!("source tombstone not found: {}", source_id.0))?
            {
                DeletionOutcome::Pending => {
                    #[cfg(feature = "qdrant")]
                    {
                        self.sync_qdrant_delete_source(&source_id).await
                    }
                    #[cfg(not(feature = "qdrant"))]
                    {
                        DeletionOutcome::Pending
                    }
                }
                outcome => outcome,
            };
            let mut report = match self.store.latest_deletion_report(&source_id)? {
                Some(previous) => previous.report,
                None => self.local_deletion_report(&source_id, DeletionOutcome::Pending)?,
            };
            report.set(DeletionProduct::Qdrant, qdrant_outcome);
            let _sqlite_write_permit =
                acquire_ingest_resource("sqlite_writer", "sqlite_write").await?;
            let mut transaction = self.store.connection().unchecked_transaction()?;
            self.store.finalize_deletion_outcome_tx(
                &mut transaction,
                &source_id,
                qdrant_outcome,
                &mut report,
            )?;
            transaction.commit()?;
            reports.push(report);
        }
        Ok(reports)
    }

    #[cfg(feature = "qdrant")]
    async fn sync_qdrant_delete_source(&self, source_id: &SourceId) -> DeletionOutcome {
        let Some(qdrant) = &self.qdrant else {
            return DeletionOutcome::Pending;
        };
        // Acquire this before the throughput permit. It excludes both in-flight
        // source/profile upserts and any later upsert from the Erased receipt's
        // remote mutation, regardless of qdrant_upsert_concurrency.
        let _qdrant_mutation_fence = qdrant_mutation_fence().write().await;
        let _qdrant_permit = match acquire_ingest_resource("qdrant_upsert", "qdrant_upsert").await {
            Ok(permit) => permit,
            Err(error) => {
                tracing::warn!(
                    source = %source_id.0,
                    error = %error,
                    "qdrant source delete could not acquire the upsert exclusion boundary"
                );
                return DeletionOutcome::Pending;
            }
        };
        match qdrant.delete_source(source_id).await {
            Ok(()) => DeletionOutcome::Erased,
            Err(err) => {
                tracing::warn!(
                    source = %source_id.0,
                    error = %err,
                    "qdrant source delete failed; local removal remains authoritative"
                );
                DeletionOutcome::Pending
            }
        }
    }

    async fn remove_source_locally(
        &mut self,
        source_id: &SourceId,
        removal_kind: SourceRemovalKind,
    ) -> Result<DeletionOutcome> {
        self.store
            .get_source(source_id)?
            .with_context(|| format!("source not found: {}", source_id.0))?;
        let active_profile_id = self.active_profile_id.clone();
        let remaining_vectors = self
            .store
            .list_vector_documents_for_profile(&active_profile_id)?
            .into_iter()
            .filter(|document| document.source_id != *source_id)
            .collect::<Vec<_>>();
        let prepared = self.prepare_indexes_from_vectors(remaining_vectors)?;
        let index_publish_permit =
            acquire_ingest_resource("index_publish", "index_publish").await?;
        let staged = self.stage_prepared_index_artifacts_for_residency(&prepared)?;
        drop(index_publish_permit);
        let sqlite_write_permit = acquire_ingest_resource("sqlite_writer", "sqlite_write").await?;
        let generation = match removal_kind {
            SourceRemovalKind::Erasure(retention_policy) => self
                .store
                .remove_source_and_replace_vectors_for_profile_with_retention(
                    &active_profile_id,
                    source_id,
                    &prepared.vectors,
                    retention_policy,
                ),
            SourceRemovalKind::Housekeeping => self
                .store
                .remove_source_and_replace_vectors_for_profile_for_housekeeping(
                    &active_profile_id,
                    source_id,
                    &prepared.vectors,
                ),
        }
        .inspect_err(|_error| {
            remove_staged_index_artifacts(&staged);
        })?;
        drop(sqlite_write_permit);
        // Clear the resident cache before the next await so a deleted id cannot
        // be served while derived artifacts and remote deletion are reconciled.
        self.invalidate_live_indexes()?;
        let index_publish_permit =
            acquire_ingest_resource("index_publish", "index_publish").await?;
        self.publish_committed_indexes(&active_profile_id, generation, staged, prepared)?;
        drop(index_publish_permit);

        match remove_source_image_artifacts(&self.data_dir, source_id) {
            Ok(()) => Ok(DeletionOutcome::Erased),
            Err(error) => {
                tracing::warn!(
                    source = %source_id.0,
                    error = %error,
                    "image artifact cleanup failed after committed source removal"
                );
                Ok(DeletionOutcome::Pending)
            }
        }
    }

    fn local_deletion_report(
        &self,
        source_id: &SourceId,
        images: DeletionOutcome,
    ) -> Result<DeletionReport> {
        let mut report = DeletionReport::new();
        for product in [
            DeletionProduct::SqliteAuthoritative,
            DeletionProduct::Chunks,
            DeletionProduct::Vectors,
            DeletionProduct::Graph,
        ] {
            report.set(product, DeletionOutcome::Erased);
        }
        // HNSW generation cleanup does not yet have a durable physical-cleanup
        // acknowledgement, so it remains retryable audit work.
        report.set(DeletionProduct::Hnsw, DeletionOutcome::Pending);
        report.set(DeletionProduct::Images, images);
        // Explicit erasure purges unreferenced content-addressed cache rows in the
        // same transaction as source removal; success is recorded as Erased.
        report.set(DeletionProduct::Caches, DeletionOutcome::Erased);
        report.set(
            DeletionProduct::Backups,
            self.store
                .backup_deletion_outcome_at(source_id, current_unix_timestamp_secs())?,
        );
        Ok(report)
    }
}

impl<E> IngestPipeline<E>
where
    E: EmbeddingClient,
{
    pub async fn remove_missing_sources_for_all_source_ingest(
        &mut self,
        task_id: Option<&TaskId>,
    ) -> Result<Vec<SourceId>> {
        self.remove_missing_sources_for_all_source_ingest_with(task_id, source_path_is_missing)
            .await
    }

    pub(crate) async fn remove_missing_sources_for_all_source_ingest_with(
        &mut self,
        task_id: Option<&TaskId>,
        mut path_is_missing: impl FnMut(&Path) -> Result<bool>,
    ) -> Result<Vec<SourceId>> {
        let missing_source_ids = self
            .store
            .list_sources()?
            .into_iter()
            .filter_map(|source| match path_is_missing(&source.path) {
                Ok(true) => Some(Ok(source.id)),
                Ok(false) => None,
                Err(error) => Some(Err(error)),
            })
            .collect::<Result<Vec<_>>>()?;
        let total = missing_source_ids.len();
        for (index, source_id) in missing_source_ids.iter().enumerate() {
            tracing::warn!(
                source = %source_id.0,
                "removing missing source before all-source ingest"
            );
            self.record_task_progress(
                task_id,
                TaskProgressSnapshot::phase(IngestTaskStage::Ingest.as_str())
                    .with_counter("missing_sources", index as u64, Some(total as u64))
                    .with_recent_status("removing missing source"),
            );
            self.remove_source_for_housekeeping(source_id)
                .await
                .with_context(|| format!("remove missing source: {}", source_id.0))?;
        }
        Ok(missing_source_ids)
    }
}

fn current_unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}
