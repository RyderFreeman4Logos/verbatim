#[cfg(feature = "qdrant")]
use std::collections::HashSet;

#[cfg(feature = "qdrant")]
use anyhow::{bail, Context, Result};

#[cfg(not(feature = "qdrant"))]
use super::IngestPipeline;
#[cfg(feature = "qdrant")]
use super::{acquire_ingest_resource, EmbeddingClient, IngestPipeline};
#[cfg(feature = "qdrant")]
use crate::index::qdrant::{records_from_store_for_profile, QdrantClient};
#[cfg(feature = "qdrant")]
use crate::types::{EmbeddingProfileId, SourceId};

#[cfg(feature = "qdrant")]
impl<E> IngestPipeline<E>
where
    E: EmbeddingClient,
{
    pub(super) async fn sync_qdrant_source(&self, source_id: &SourceId) {
        self.sync_qdrant_profile_source(&self.active_profile_id, source_id)
            .await;
    }

    pub async fn sync_pending_qdrant_profile_resets(&mut self) {
        let profiles = std::mem::take(&mut self.pending_qdrant_profile_syncs);
        for profile_id in profiles {
            self.sync_qdrant_profile_all(&profile_id).await;
        }
    }

    pub(super) async fn sync_qdrant_profile_source(
        &self,
        profile_id: &EmbeddingProfileId,
        source_id: &SourceId,
    ) {
        let Some(qdrant) = &self.qdrant else {
            return;
        };
        let result: Result<()> = async {
            let records = records_from_store_for_profile(&self.store, profile_id, Some(source_id))?;
            // Source deletion takes this same permit, so once this critical section
            // begins its durable outcome cannot advance to Erased behind a later
            // upsert. A process exit therefore leaves either no upsert or a pending
            // tombstone for the deletion/reconcile path to finish.
            let _qdrant_permit = acquire_ingest_resource("qdrant_upsert", "qdrant_upsert").await?;
            if self.store.is_tombstoned(source_id)? {
                return Ok(());
            }
            qdrant
                .delete_source_for_profile(profile_id, source_id)
                .await?;
            if self.store.is_tombstoned(source_id)? {
                return Ok(());
            }
            qdrant.upsert_records(&records).await?;
            self.compensate_tombstoned_qdrant_source(qdrant, profile_id, source_id)
                .await
        }
        .await;
        if let Err(err) = result {
            tracing::warn!(
                embedding_profile_id = %profile_id,
                source = %source_id.0,
                error = %err,
                "qdrant source sync failed; local ingest remains authoritative"
            );
        }
    }

    pub(super) async fn sync_qdrant_all(&self) {
        self.sync_qdrant_profile_all(&self.active_profile_id).await;
    }

    pub(super) async fn sync_qdrant_profile_all(&self, profile_id: &EmbeddingProfileId) {
        let Some(qdrant) = &self.qdrant else {
            return;
        };
        let result: Result<()> = async {
            let mut records = records_from_store_for_profile(&self.store, profile_id, None)?;
            let source_ids = records
                .iter()
                .map(|record| record.document.source_id.clone())
                .collect::<HashSet<_>>();
            // Source deletion takes this same permit. Filtering under the permit
            // closes the stale-read window before the replacement upsert, while the
            // post-upsert compensation covers tombstones committed during its await.
            let _qdrant_permit = acquire_ingest_resource("qdrant_upsert", "qdrant_upsert").await?;
            let mut tombstoned_source_ids = HashSet::new();
            for source_id in &source_ids {
                if self.store.is_tombstoned(source_id)? {
                    tombstoned_source_ids.insert(source_id.clone());
                }
            }
            records.retain(|record| !tombstoned_source_ids.contains(&record.document.source_id));
            qdrant.delete_profile(profile_id).await?;
            qdrant.upsert_records(&records).await?;

            let mut compensation_errors = Vec::new();
            for source_id in source_ids {
                if let Err(error) = self
                    .compensate_tombstoned_qdrant_source(qdrant, profile_id, &source_id)
                    .await
                {
                    compensation_errors.push(format!("{}: {error:#}", source_id.0));
                }
            }
            if !compensation_errors.is_empty() {
                bail!(
                    "qdrant full-profile tombstone compensation failed: {}",
                    compensation_errors.join("; ")
                );
            }
            Ok(())
        }
        .await;
        if let Err(err) = result {
            tracing::warn!(
                embedding_profile_id = %profile_id,
                error = %err,
                "qdrant full sync failed; local indexes remain authoritative"
            );
        }
    }

    async fn compensate_tombstoned_qdrant_source(
        &self,
        qdrant: &QdrantClient,
        profile_id: &EmbeddingProfileId,
        source_id: &SourceId,
    ) -> Result<()> {
        if !self.store.is_tombstoned(source_id)? {
            return Ok(());
        }
        if let Err(delete_error) = qdrant
            .delete_source_for_profile(profile_id, source_id)
            .await
        {
            self.requeue_qdrant_deletion_after_failed_compensation(source_id)
                .await
                .with_context(|| {
                    format!(
                        "requeue Qdrant deletion after failed compensation for {}: {delete_error:#}",
                        source_id.0
                    )
                })?;
            return Err(delete_error).context("compensate Qdrant upsert after source deletion");
        }
        Ok(())
    }

    async fn requeue_qdrant_deletion_after_failed_compensation(
        &self,
        source_id: &SourceId,
    ) -> Result<()> {
        let _sqlite_write_permit = acquire_ingest_resource("sqlite_writer", "sqlite_write").await?;
        self.store.requeue_qdrant_deletion(source_id)
    }
}

#[cfg(not(feature = "qdrant"))]
impl<E> IngestPipeline<E>
where
    E: super::EmbeddingClient,
{
    pub async fn sync_pending_qdrant_profile_resets(&mut self) {}
}
