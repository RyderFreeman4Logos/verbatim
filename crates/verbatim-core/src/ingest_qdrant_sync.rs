#[cfg(feature = "qdrant")]
use std::collections::HashSet;

#[cfg(feature = "qdrant")]
use anyhow::{bail, Context, Result};

#[cfg(not(feature = "qdrant"))]
use super::IngestPipeline;
#[cfg(feature = "qdrant")]
use super::{acquire_ingest_resource, qdrant_mutation_fence, EmbeddingClient, IngestPipeline};
#[cfg(feature = "qdrant")]
use crate::index::qdrant::{records_from_store_for_profile, QdrantClient, QdrantVectorRecord};
#[cfg(feature = "qdrant")]
use crate::store::Store;
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
            let Some(database_path) = self.store.database_path().map(ToOwned::to_owned) else {
                return sync_qdrant_profile_source_mutation(
                    |source_id| self.store.is_tombstoned(source_id),
                    |source_id| self.store.requeue_qdrant_deletion(source_id),
                    qdrant,
                    profile_id,
                    source_id,
                    records,
                )
                .await;
            };
            let task_qdrant = qdrant.clone();
            let task_profile_id = profile_id.clone();
            let task_source_id = source_id.clone();
            let task_records = records.clone();
            let task_durability_profile = self.store.durability_profile();
            #[cfg(test)]
            let task_requeue_store_observer = self.qdrant_requeue_store_observer.clone();
            tokio::spawn(async move {
                let tombstone_database_path = database_path.clone();
                sync_qdrant_profile_source_mutation(
                    move |source_id| {
                        let store = Store::open_existing_readonly(&tombstone_database_path)?;
                        store.is_tombstoned(source_id)
                    },
                    move |source_id| {
                        let store = Store::new_with_durability_profile(
                            &database_path,
                            task_durability_profile,
                        )?;
                        #[cfg(test)]
                        if let Some(observer) = &task_requeue_store_observer {
                            observer(&store);
                        }
                        store.requeue_qdrant_deletion(source_id)
                    },
                    &task_qdrant,
                    &task_profile_id,
                    &task_source_id,
                    task_records,
                )
                .await
            })
            .await
            .context("join cancellation-safe Qdrant source sync task")?
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
            let records = records_from_store_for_profile(&self.store, profile_id, None)?;
            let Some(database_path) = self.store.database_path().map(ToOwned::to_owned) else {
                return sync_qdrant_profile_all_mutation(
                    |source_id| self.store.is_tombstoned(source_id),
                    |source_id| self.store.requeue_qdrant_deletion(source_id),
                    qdrant,
                    profile_id,
                    records,
                )
                .await;
            };
            let task_qdrant = qdrant.clone();
            let task_profile_id = profile_id.clone();
            let task_records = records.clone();
            let task_durability_profile = self.store.durability_profile();
            #[cfg(test)]
            let task_requeue_store_observer = self.qdrant_requeue_store_observer.clone();
            tokio::spawn(async move {
                let tombstone_database_path = database_path.clone();
                sync_qdrant_profile_all_mutation(
                    move |source_id| {
                        let store = Store::open_existing_readonly(&tombstone_database_path)?;
                        store.is_tombstoned(source_id)
                    },
                    move |source_id| {
                        let store = Store::new_with_durability_profile(
                            &database_path,
                            task_durability_profile,
                        )?;
                        #[cfg(test)]
                        if let Some(observer) = &task_requeue_store_observer {
                            observer(&store);
                        }
                        store.requeue_qdrant_deletion(source_id)
                    },
                    &task_qdrant,
                    &task_profile_id,
                    task_records,
                )
                .await
            })
            .await
            .context("join cancellation-safe Qdrant full-profile sync task")?
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
}

#[cfg(feature = "qdrant")]
async fn sync_qdrant_profile_source_mutation<F, R>(
    is_tombstoned: F,
    requeue_deletion: R,
    qdrant: &QdrantClient,
    profile_id: &EmbeddingProfileId,
    source_id: &SourceId,
    records: Vec<QdrantVectorRecord>,
) -> Result<()>
where
    F: Fn(&SourceId) -> Result<bool>,
    R: Fn(&SourceId) -> Result<()>,
{
    // The independent task owns this shared guard, so caller cancellation cannot
    // allow a deletion receipt to pass an already-admitted remote mutation.
    let _qdrant_mutation_fence = qdrant_mutation_fence().read().await;
    let _qdrant_permit = acquire_ingest_resource("qdrant_upsert", "qdrant_upsert").await?;
    if is_tombstoned(source_id)? {
        return Ok(());
    }
    qdrant
        .delete_source_for_profile(profile_id, source_id)
        .await?;
    if is_tombstoned(source_id)? {
        return Ok(());
    }
    let upsert_result = qdrant.upsert_records(&records).await;
    if let Err(error) = &upsert_result {
        tracing::warn!(
            embedding_profile_id = %profile_id,
            source = %source_id.0,
            error = %error,
            "qdrant source upsert failed; checking tombstone compensation"
        );
    }
    compensate_tombstoned_qdrant_source(
        &is_tombstoned,
        &requeue_deletion,
        qdrant,
        profile_id,
        source_id,
    )
    .await?;
    upsert_result
}

#[cfg(feature = "qdrant")]
async fn sync_qdrant_profile_all_mutation<F, R>(
    is_tombstoned: F,
    requeue_deletion: R,
    qdrant: &QdrantClient,
    profile_id: &EmbeddingProfileId,
    mut records: Vec<QdrantVectorRecord>,
) -> Result<()>
where
    F: Fn(&SourceId) -> Result<bool>,
    R: Fn(&SourceId) -> Result<()>,
{
    let source_ids = records
        .iter()
        .map(|record| record.document.source_id.clone())
        .collect::<HashSet<_>>();
    // Keep the full-profile replacement and every tombstone compensation behind
    // this guard so an exclusive deletion cannot record Erased before the task's
    // stale upsert has completed or been compensated.
    let _qdrant_mutation_fence = qdrant_mutation_fence().read().await;
    let _qdrant_permit = acquire_ingest_resource("qdrant_upsert", "qdrant_upsert").await?;
    let mut tombstoned_source_ids = HashSet::new();
    for source_id in &source_ids {
        if is_tombstoned(source_id)? {
            tombstoned_source_ids.insert(source_id.clone());
        }
    }
    records.retain(|record| !tombstoned_source_ids.contains(&record.document.source_id));
    qdrant.delete_profile(profile_id).await?;
    let upsert_result = qdrant.upsert_records(&records).await;
    if let Err(error) = &upsert_result {
        tracing::warn!(
            embedding_profile_id = %profile_id,
            error = %error,
            "qdrant full-profile upsert failed; checking tombstone compensation"
        );
    }

    let mut compensation_errors = Vec::new();
    for source_id in source_ids {
        if let Err(error) = compensate_tombstoned_qdrant_source(
            &is_tombstoned,
            &requeue_deletion,
            qdrant,
            profile_id,
            &source_id,
        )
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
    upsert_result
}

#[cfg(feature = "qdrant")]
async fn compensate_tombstoned_qdrant_source<F, R>(
    is_tombstoned: &F,
    requeue_deletion: &R,
    qdrant: &QdrantClient,
    profile_id: &EmbeddingProfileId,
    source_id: &SourceId,
) -> Result<()>
where
    F: Fn(&SourceId) -> Result<bool>,
    R: Fn(&SourceId) -> Result<()>,
{
    if !is_tombstoned(source_id)? {
        return Ok(());
    }
    if let Err(delete_error) = qdrant
        .delete_source_for_profile(profile_id, source_id)
        .await
    {
        let _sqlite_write_permit = acquire_ingest_resource("sqlite_writer", "sqlite_write").await?;
        requeue_deletion(source_id).with_context(|| {
            format!(
                "requeue Qdrant deletion after failed compensation for {}: {delete_error:#}",
                source_id.0
            )
        })?;
        return Err(delete_error).context("compensate Qdrant upsert after source deletion");
    }
    Ok(())
}

#[cfg(not(feature = "qdrant"))]
impl<E> IngestPipeline<E>
where
    E: super::EmbeddingClient,
{
    pub async fn sync_pending_qdrant_profile_resets(&mut self) {}
}
