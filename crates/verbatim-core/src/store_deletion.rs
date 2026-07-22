use std::time::{SystemTime, UNIX_EPOCH};

use anyhow::{bail, Context, Result};
use rusqlite::{params, OptionalExtension, Transaction};

use super::{
    bump_all_profile_index_generations, map_storage_error, replace_vector_documents_for_profile_tx,
    unix_timestamp_string, SqliteWriteOperation, Store,
};
use crate::deletion::{
    DeletionOutcome, DeletionProduct, DeletionReport, PersistedDeletionReport, RetentionPolicy,
};
use crate::traits::VectorDocument;
use crate::types::{EmbeddingProfileId, SourceId};

pub(super) const DELETION_SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS source_tombstones (
    source_id TEXT PRIMARY KEY,
    deleted_at TEXT NOT NULL,
    backup_expiry_at INTEGER,
    legal_hold INTEGER NOT NULL DEFAULT 0,
    qdrant_outcome TEXT NOT NULL DEFAULT 'pending',
    last_reconcile_attempt_ts INTEGER
);
CREATE INDEX IF NOT EXISTS source_tombstones_qdrant_outcome_idx
    ON source_tombstones(qdrant_outcome);
CREATE TABLE IF NOT EXISTS deletion_reports (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    source_id TEXT NOT NULL,
    recorded_at TEXT NOT NULL,
    retention_policy_json TEXT NOT NULL,
    outcomes_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS deletion_reports_source_id_idx
    ON deletion_reports(source_id, id);
"#;

pub(super) const PREVENT_RESURRECTED_SOURCES_TRIGGER: &str = r#"
CREATE TRIGGER IF NOT EXISTS prevent_resurrected_sources
BEFORE INSERT ON sources
FOR EACH ROW
WHEN EXISTS (SELECT 1 FROM source_tombstones WHERE source_id = NEW.id)
BEGIN
    SELECT RAISE(ABORT, 'source is tombstoned');
END;
"#;

impl Store {
    /// Return whether a source id has a durable deletion tombstone.
    pub fn is_tombstoned(&self, source_id: &SourceId) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM source_tombstones WHERE source_id = ?1",
            params![&source_id.0],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Place a legal hold on the retained backup lifecycle for a deleted source.
    pub fn place_legal_hold(&self, source_id: &SourceId) -> Result<()> {
        self.set_legal_hold(source_id, true)
    }

    /// Release a legal hold while preserving the source tombstone.
    pub fn release_legal_hold(&self, source_id: &SourceId) -> Result<()> {
        self.set_legal_hold(source_id, false)
    }

    /// Replace the backup-retention policy recorded for a tombstoned source.
    pub fn set_retention_policy(
        &self,
        source_id: &SourceId,
        policy: RetentionPolicy,
    ) -> Result<()> {
        let (backup_expiry_at, legal_hold) = tombstone_retention_values(policy)?;
        let mut transaction = self.conn.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE source_tombstones
             SET backup_expiry_at = ?2, legal_hold = ?3
             WHERE source_id = ?1",
            params![&source_id.0, backup_expiry_at, legal_hold],
        )?;
        if changed == 0 {
            bail!("source tombstone not found: {}", source_id.0);
        }
        self.refresh_latest_deletion_report_tx(&mut transaction, source_id, policy)?;
        transaction.commit()?;
        Ok(())
    }

    /// Read the effective backup-retention policy for a tombstoned source.
    pub fn retention_policy(&self, source_id: &SourceId) -> Result<Option<RetentionPolicy>> {
        let state = self
            .conn
            .query_row(
                "SELECT backup_expiry_at, legal_hold
                 FROM source_tombstones WHERE source_id = ?1",
                params![&source_id.0],
                |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, bool>(1)?)),
            )
            .optional()?;
        state.map(tombstone_retention_policy).transpose()
    }

    /// Evaluate whether retained backup material may be terminally erased at `now`.
    pub fn backup_deletion_outcome_at(
        &self,
        source_id: &SourceId,
        now: u64,
    ) -> Result<DeletionOutcome> {
        Ok(self
            .retention_policy(source_id)?
            .map_or(DeletionOutcome::NotFound, |policy| {
                policy.backup_outcome_at(now)
            }))
    }

    #[cfg(test)]
    pub(crate) fn pending_qdrant_deletion_source_ids(&self) -> Result<Vec<SourceId>> {
        let mut statement = self.conn.prepare(
            "SELECT source_id FROM source_tombstones
             WHERE qdrant_outcome = 'pending' ORDER BY source_id",
        )?;
        let rows = statement.query_map([], |row| Ok(SourceId(row.get(0)?)))?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    /// List tombstones that still need Qdrant or retained-backup reconciliation.
    pub(crate) fn reconciliation_deletion_source_ids(&self) -> Result<Vec<SourceId>> {
        self.reconciliation_deletion_source_ids_with_limit(None)
    }

    /// List no more than `max_sources` tombstones for one bounded reconciliation batch.
    pub(crate) fn reconciliation_deletion_source_ids_up_to(
        &self,
        max_sources: usize,
    ) -> Result<Vec<SourceId>> {
        self.reconciliation_deletion_source_ids_with_limit(Some(max_sources))
    }

    fn reconciliation_deletion_source_ids_with_limit(
        &self,
        max_sources: Option<usize>,
    ) -> Result<Vec<SourceId>> {
        // Keep candidate selection on the tombstone table. The previously joined
        // `deletion_reports` history was O(history) before applying a batch limit.
        // `last_reconcile_attempt_ts` lets the index order new work before retries
        // while the LIMIT bounds one startup/reconcile round to its batch.
        let sql = "
            SELECT source_id
            FROM source_tombstones
            WHERE (
                    qdrant_outcome = 'pending'
                    OR legal_hold = 1
                    OR backup_expiry_at IS NOT NULL
                  )
              AND (
                    qdrant_outcome = 'pending'
                    OR legal_hold = 1
                    OR last_reconcile_attempt_ts IS NULL
                    OR last_reconcile_attempt_ts < backup_expiry_at
                  )
            ORDER BY last_reconcile_attempt_ts, source_id
        ";
        let mut statement = match max_sources {
            Some(max_sources) => {
                let limited = format!("{sql} LIMIT ?1");
                let mut statement = self.conn.prepare(&limited)?;
                let limit = i64::try_from(max_sources).unwrap_or(i64::MAX);
                let rows = statement.query_map(params![limit], |row| Ok(SourceId(row.get(0)?)))?;
                return rows.map(|row| row.map_err(Into::into)).collect();
            }
            None => self.conn.prepare(sql)?,
        };
        let rows = statement.query_map([], |row| Ok(SourceId(row.get(0)?)))?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    /// Return the durable Qdrant outcome for a tombstoned source.
    pub(crate) fn qdrant_deletion_outcome(
        &self,
        source_id: &SourceId,
    ) -> Result<Option<DeletionOutcome>> {
        let outcome = self
            .conn
            .query_row(
                "SELECT qdrant_outcome FROM source_tombstones WHERE source_id = ?1",
                params![&source_id.0],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        outcome
            .map(|outcome| qdrant_outcome_from_str(&outcome))
            .transpose()
    }

    /// Persist a Qdrant deletion outcome and an audit receipt from the current tombstone policy.
    pub(crate) fn finalize_deletion_outcome_tx(
        &self,
        transaction: &mut Transaction<'_>,
        source_id: &SourceId,
        outcome: DeletionOutcome,
        report: &mut DeletionReport,
    ) -> Result<()> {
        let retention_policy = retention_policy_in_transaction(transaction, source_id)?;
        report.set(
            DeletionProduct::Backups,
            retention_policy.backup_outcome_at(current_unix_timestamp_secs()),
        );
        let last_reconcile_attempt_ts =
            i64::try_from(current_unix_timestamp_secs()).unwrap_or(i64::MAX);
        let changed = transaction.execute(
            "UPDATE source_tombstones
             SET qdrant_outcome = ?2, last_reconcile_attempt_ts = ?3
             WHERE source_id = ?1",
            params![
                &source_id.0,
                qdrant_outcome_to_str(outcome),
                last_reconcile_attempt_ts,
            ],
        )?;
        if changed == 0 {
            bail!("source tombstone not found: {}", source_id.0);
        }

        self.persist_deletion_report_tx(transaction, source_id, retention_policy, report)
    }

    /// Append a durable, content-free receipt for an erasure attempt.
    pub fn persist_deletion_report(
        &self,
        source_id: &SourceId,
        retention_policy: RetentionPolicy,
        report: &DeletionReport,
    ) -> Result<()> {
        let mut transaction = self.conn.unchecked_transaction()?;
        self.persist_deletion_report_tx(&mut transaction, source_id, retention_policy, report)?;
        transaction.commit()?;
        Ok(())
    }

    fn persist_deletion_report_tx(
        &self,
        transaction: &mut Transaction<'_>,
        source_id: &SourceId,
        retention_policy: RetentionPolicy,
        report: &DeletionReport,
    ) -> Result<()> {
        let retention_policy_json = serde_json::to_string(&retention_policy)
            .context("serialize deletion-report retention policy")?;
        let outcomes_json =
            serde_json::to_string(report).context("serialize deletion-report outcomes")?;
        transaction.execute(
            "INSERT INTO deletion_reports
             (source_id, recorded_at, retention_policy_json, outcomes_json)
             VALUES (?1, ?2, ?3, ?4)",
            params![
                &source_id.0,
                unix_timestamp_string(),
                retention_policy_json,
                outcomes_json,
            ],
        )?;
        Ok(())
    }

    fn refresh_latest_deletion_report_tx(
        &self,
        transaction: &mut Transaction<'_>,
        source_id: &SourceId,
        retention_policy: RetentionPolicy,
    ) -> Result<()> {
        let outcomes_json = transaction
            .query_row(
                "SELECT outcomes_json FROM deletion_reports
                 WHERE source_id = ?1 ORDER BY id DESC LIMIT 1",
                params![&source_id.0],
                |row| row.get::<_, String>(0),
            )
            .optional()?;
        let Some(outcomes_json) = outcomes_json else {
            return Ok(());
        };
        let mut report: DeletionReport = serde_json::from_str(&outcomes_json)
            .context("deserialize latest deletion-report outcomes")?;
        report.set(
            DeletionProduct::Backups,
            retention_policy.backup_outcome_at(current_unix_timestamp_secs()),
        );
        self.persist_deletion_report_tx(transaction, source_id, retention_policy, &report)
    }

    /// List durable deletion receipts in insertion order for audit and reconciliation status.
    pub fn list_deletion_reports(&self) -> Result<Vec<PersistedDeletionReport>> {
        let mut statement = self.conn.prepare(
            "SELECT source_id, recorded_at, retention_policy_json, outcomes_json
             FROM deletion_reports ORDER BY id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
            ))
        })?;
        rows.map(|row| {
            let (source_id, recorded_at, retention_policy_json, outcomes_json) = row?;
            deletion_report_from_storage(
                source_id,
                recorded_at,
                retention_policy_json,
                outcomes_json,
            )
        })
        .collect()
    }

    pub(crate) fn latest_deletion_report(
        &self,
        source_id: &SourceId,
    ) -> Result<Option<PersistedDeletionReport>> {
        let report = self
            .conn
            .query_row(
                "SELECT source_id, recorded_at, retention_policy_json, outcomes_json
                 FROM deletion_reports WHERE source_id = ?1 ORDER BY id DESC LIMIT 1",
                params![&source_id.0],
                |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .optional()?;
        report
            .map(
                |(source_id, recorded_at, retention_policy_json, outcomes_json)| {
                    deletion_report_from_storage(
                        source_id,
                        recorded_at,
                        retention_policy_json,
                        outcomes_json,
                    )
                },
            )
            .transpose()
    }

    pub fn remove_source(&self, id: &SourceId) -> Result<u64> {
        self.remove_source_with_retention(id, RetentionPolicy::Immediate)
    }

    /// Remove a source during ordinary filesystem or collection housekeeping.
    ///
    /// Unlike an explicit erasure, this deliberately leaves the source id reusable.
    pub fn remove_source_for_housekeeping(&self, id: &SourceId) -> Result<u64> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM sources WHERE id = ?1", params![&id.0])?;
        let generation = bump_all_profile_index_generations(&tx)?;
        tx.commit()?;
        Ok(generation)
    }

    pub fn remove_source_with_retention(
        &self,
        id: &SourceId,
        retention_policy: RetentionPolicy,
    ) -> Result<u64> {
        let tx = self.conn.unchecked_transaction()?;
        // Delete first so CASCADE drops this source's live rows; then anti-join
        // purge every content-addressed cache row no remaining live source owns.
        // This catches historical V1 hashes after V2 replace and pre-commit orphans.
        tx.execute("DELETE FROM sources WHERE id = ?1", params![&id.0])?;
        record_source_tombstone(&tx, id, retention_policy)?;
        purge_unreferenced_caches_tx(&tx)?;
        let generation = bump_all_profile_index_generations(&tx)?;
        tx.commit()?;
        Ok(generation)
    }

    pub fn remove_source_and_replace_vectors_for_profile(
        &self,
        profile_id: &EmbeddingProfileId,
        id: &SourceId,
        vectors: &[VectorDocument],
    ) -> Result<u64> {
        self.remove_source_and_replace_vectors_for_profile_with_retention(
            profile_id,
            id,
            vectors,
            RetentionPolicy::Immediate,
        )
    }

    /// Replace a source's derived vectors while removing a missing source without tombstoning it.
    pub fn remove_source_and_replace_vectors_for_profile_for_housekeeping(
        &self,
        profile_id: &EmbeddingProfileId,
        id: &SourceId,
        vectors: &[VectorDocument],
    ) -> Result<u64> {
        self.ensure_write_capacity(SqliteWriteOperation::Ingest)?;
        (|| {
            let tx = self.conn.unchecked_transaction()?;
            tx.execute("DELETE FROM sources WHERE id = ?1", params![&id.0])?;
            replace_vector_documents_for_profile_tx(&tx, profile_id, vectors)?;
            let generation = bump_all_profile_index_generations(&tx)?;
            tx.commit()?;
            Ok(generation)
        })()
        .map_err(|error| map_storage_error(SqliteWriteOperation::Ingest, error))
    }

    pub fn remove_source_and_replace_vectors_for_profile_with_retention(
        &self,
        profile_id: &EmbeddingProfileId,
        id: &SourceId,
        vectors: &[VectorDocument],
        retention_policy: RetentionPolicy,
    ) -> Result<u64> {
        self.ensure_write_capacity(SqliteWriteOperation::Ingest)?;
        (|| {
            let tx = self.conn.unchecked_transaction()?;
            // Delete first so CASCADE drops this source's live rows; then anti-join
            // purge every content-addressed cache row no remaining live source owns.
            // This catches historical V1 hashes after V2 replace and pre-commit orphans.
            tx.execute("DELETE FROM sources WHERE id = ?1", params![&id.0])?;
            record_source_tombstone(&tx, id, retention_policy)?;
            purge_unreferenced_caches_tx(&tx)?;
            replace_vector_documents_for_profile_tx(&tx, profile_id, vectors)?;
            let generation = bump_all_profile_index_generations(&tx)?;
            tx.commit()?;
            Ok(generation)
        })()
        .map_err(|error| map_storage_error(SqliteWriteOperation::Ingest, error))
    }

    fn set_legal_hold(&self, source_id: &SourceId, legal_hold: bool) -> Result<()> {
        let mut transaction = self.conn.unchecked_transaction()?;
        let changed = transaction.execute(
            "UPDATE source_tombstones SET legal_hold = ?2 WHERE source_id = ?1",
            params![&source_id.0, legal_hold],
        )?;
        if changed == 0 {
            bail!("source tombstone not found: {}", source_id.0);
        }
        let retention_policy = retention_policy_in_transaction(&transaction, source_id)?;
        self.refresh_latest_deletion_report_tx(&mut transaction, source_id, retention_policy)?;
        transaction.commit()?;
        Ok(())
    }
}

fn deletion_report_from_storage(
    source_id: String,
    recorded_at: String,
    retention_policy_json: String,
    outcomes_json: String,
) -> Result<PersistedDeletionReport> {
    Ok(PersistedDeletionReport {
        source_id: SourceId(source_id),
        recorded_at,
        retention_policy: serde_json::from_str(&retention_policy_json)
            .context("deserialize deletion-report retention policy")?,
        report: serde_json::from_str(&outcomes_json)
            .context("deserialize deletion-report outcomes")?,
    })
}

fn tombstone_retention_values(policy: RetentionPolicy) -> Result<(Option<i64>, bool)> {
    match policy {
        RetentionPolicy::Immediate => Ok((None, false)),
        RetentionPolicy::UntilBackupExpiry(expiry) => {
            let expiry = i64::try_from(expiry).context("backup expiry exceeds SQLite range")?;
            Ok((Some(expiry), false))
        }
        RetentionPolicy::LegalHold => Ok((None, true)),
    }
}

fn tombstone_retention_policy(state: (Option<i64>, bool)) -> Result<RetentionPolicy> {
    let (backup_expiry_at, legal_hold) = state;
    if legal_hold {
        return Ok(RetentionPolicy::LegalHold);
    }
    match backup_expiry_at {
        Some(expiry) => Ok(RetentionPolicy::UntilBackupExpiry(
            u64::try_from(expiry).context("negative backup expiry in tombstone")?,
        )),
        None => Ok(RetentionPolicy::Immediate),
    }
}

fn retention_policy_in_transaction(
    transaction: &Transaction<'_>,
    source_id: &SourceId,
) -> Result<RetentionPolicy> {
    let state = transaction
        .query_row(
            "SELECT backup_expiry_at, legal_hold
             FROM source_tombstones WHERE source_id = ?1",
            params![&source_id.0],
            |row| Ok((row.get::<_, Option<i64>>(0)?, row.get::<_, bool>(1)?)),
        )
        .optional()?
        .with_context(|| format!("source tombstone not found: {}", source_id.0))?;
    tombstone_retention_policy(state)
}

fn current_unix_timestamp_secs() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs())
}

fn record_source_tombstone(
    transaction: &Transaction<'_>,
    source_id: &SourceId,
    retention_policy: RetentionPolicy,
) -> Result<()> {
    let (backup_expiry_at, legal_hold) = tombstone_retention_values(retention_policy)?;
    transaction.execute(
        "INSERT INTO source_tombstones
         (source_id, deleted_at, backup_expiry_at, legal_hold, qdrant_outcome)
         VALUES (?1, ?2, ?3, ?4, 'pending')
         ON CONFLICT(source_id) DO NOTHING",
        params![
            &source_id.0,
            unix_timestamp_string(),
            backup_expiry_at,
            legal_hold,
        ],
    )?;
    Ok(())
}

/// Purge content-addressed cache rows that no remaining live source references.
///
/// Uses anti-joins against *all* live chunks / image_artifacts, not only hashes
/// collected from the deleted source's current rows. Explicit erasure must not
/// leave historical V1 cache entries (after a V2 replace) or pre-commit orphan
/// rows that never gained a live owner.
fn purge_unreferenced_caches_tx(transaction: &Transaction<'_>) -> Result<()> {
    // Ensure supporting indexes exist for the anti-join (idempotent).
    // Created here rather than in DELETION_SCHEMA because some legacy test
    // fixtures open a reduced schema without these columns.
    transaction.execute_batch(
        "CREATE INDEX IF NOT EXISTS chunks_embedding_input_hash_idx \
         ON chunks(embedding_input_hash) \
         WHERE embedding_input_hash IS NOT NULL AND embedding_input_hash != ''",
    )?;
    transaction.execute_batch(
        "CREATE INDEX IF NOT EXISTS image_artifacts_content_hash_idx \
         ON image_artifacts(content_hash) \
         WHERE content_hash != ''",
    )?;
    transaction.execute(
        "DELETE FROM embedding_cache
         WHERE NOT EXISTS (
             SELECT 1
             FROM chunks
             WHERE chunks.embedding_input_hash = embedding_cache.embedding_input_hash
               AND chunks.embedding_input_hash IS NOT NULL
               AND chunks.embedding_input_hash != ''
         )",
        [],
    )?;
    transaction.execute(
        "DELETE FROM image_captions
         WHERE NOT EXISTS (
             SELECT 1
             FROM image_artifacts
             WHERE image_artifacts.content_hash = image_captions.image_hash
               AND image_artifacts.content_hash != ''
         )",
        [],
    )?;
    Ok(())
}

fn qdrant_outcome_to_str(outcome: DeletionOutcome) -> &'static str {
    match outcome {
        DeletionOutcome::Erased => "erased",
        DeletionOutcome::Pending => "pending",
        DeletionOutcome::Held => "held",
        DeletionOutcome::NotFound => "not_found",
    }
}

fn qdrant_outcome_from_str(outcome: &str) -> Result<DeletionOutcome> {
    match outcome {
        "erased" => Ok(DeletionOutcome::Erased),
        "pending" => Ok(DeletionOutcome::Pending),
        "held" => Ok(DeletionOutcome::Held),
        "not_found" => Ok(DeletionOutcome::NotFound),
        _ => bail!("unknown qdrant deletion outcome: {outcome}"),
    }
}
