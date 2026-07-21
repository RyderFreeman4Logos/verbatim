use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{Context, Result};

use super::{
    acquire_ingest_resource, remove_source_image_artifacts, remove_staged_index_artifacts,
    EmbeddingClient, IngestPipeline,
};
use crate::deletion::{DeletionOutcome, DeletionProduct, DeletionReport, RetentionPolicy};
use crate::types::SourceId;

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
        let generation = match self
            .store
            .remove_source_and_replace_vectors_for_profile_with_retention(
                &active_profile_id,
                source_id,
                &prepared.vectors,
                retention_policy,
            ) {
            Ok(generation) => generation,
            Err(err) => {
                remove_staged_index_artifacts(&staged);
                return Err(err);
            }
        };
        drop(sqlite_write_permit);
        // Clear the resident cache before the next await so a deleted id cannot
        // be served while derived artifacts and remote deletion are reconciled.
        self.invalidate_live_indexes()?;
        let index_publish_permit =
            acquire_ingest_resource("index_publish", "index_publish").await?;
        self.publish_committed_indexes(&active_profile_id, generation, staged, prepared)?;
        drop(index_publish_permit);
        remove_source_image_artifacts(&self.data_dir, source_id).with_context(|| {
            format!(
                "cleanup image artifacts after committed source removal: {}",
                source_id.0
            )
        })?;

        #[cfg(feature = "qdrant")]
        let qdrant_outcome = self.sync_qdrant_delete_source(source_id).await;
        #[cfg(not(feature = "qdrant"))]
        let qdrant_outcome = DeletionOutcome::NotFound;
        self.store
            .set_qdrant_deletion_outcome(source_id, qdrant_outcome)?;

        let mut report = self.local_deletion_report(source_id)?;
        report.set(DeletionProduct::Qdrant, qdrant_outcome);
        Ok(report)
    }

    /// Retry remote deletion for every source whose authoritative local erasure completed.
    pub async fn reconcile_deletions(&self) -> Result<Vec<DeletionReport>> {
        let source_ids = self.store.pending_qdrant_deletion_source_ids()?;
        let mut reports = Vec::with_capacity(source_ids.len());
        for source_id in source_ids {
            #[cfg(feature = "qdrant")]
            let qdrant_outcome = self.sync_qdrant_delete_source(&source_id).await;
            #[cfg(not(feature = "qdrant"))]
            let qdrant_outcome = DeletionOutcome::NotFound;
            self.store
                .set_qdrant_deletion_outcome(&source_id, qdrant_outcome)?;
            let mut report = self.local_deletion_report(&source_id)?;
            report.set(DeletionProduct::Qdrant, qdrant_outcome);
            reports.push(report);
        }
        Ok(reports)
    }

    #[cfg(feature = "qdrant")]
    async fn sync_qdrant_delete_source(&self, source_id: &SourceId) -> DeletionOutcome {
        let Some(qdrant) = &self.qdrant else {
            return DeletionOutcome::NotFound;
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

    fn local_deletion_report(&self, source_id: &SourceId) -> Result<DeletionReport> {
        let mut report = DeletionReport::new();
        for product in [
            DeletionProduct::SqliteAuthoritative,
            DeletionProduct::Chunks,
            DeletionProduct::Vectors,
            DeletionProduct::Hnsw,
            DeletionProduct::Graph,
            DeletionProduct::Images,
            DeletionProduct::Caches,
        ] {
            report.set(product, DeletionOutcome::Erased);
        }
        report.set(
            DeletionProduct::Backups,
            self.store
                .backup_deletion_outcome_at(source_id, current_unix_timestamp_secs())?,
        );
        Ok(report)
    }
}

fn current_unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}
