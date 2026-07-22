use anyhow::{Context, Result};
use rusqlite::params;

use super::{
    image_caption_status_to_str, sql_usize, unix_timestamp_string, vector_to_blob,
    EmbeddingCacheEntry, EmbeddingCacheVector, EmbeddingProfileId, SourceId, Store,
};
use crate::vision_caption::CaptionAttempt;

/// One embedding-cache write paired with the source whose tombstone fences it.
#[derive(Debug, Clone, Copy)]
pub struct SourceEmbeddingCacheVector<'a> {
    pub source_id: &'a SourceId,
    pub embedding_input_hash: &'a str,
    pub vector: &'a [f32],
}

impl Store {
    pub fn upsert_embedding_cache_entries(
        &self,
        profile_id: &EmbeddingProfileId,
        profile_config_hash: &str,
        entries: &[EmbeddingCacheEntry],
    ) -> Result<()> {
        let vectors = entries
            .iter()
            .map(|entry| EmbeddingCacheVector {
                embedding_input_hash: &entry.embedding_input_hash,
                vector: &entry.vector,
            })
            .collect::<Vec<_>>();
        self.upsert_embedding_cache_vectors(profile_id, profile_config_hash, &vectors)
    }

    /// Upsert cache rows from borrowed vectors without taking ownership of the payloads.
    pub fn upsert_embedding_cache_vectors(
        &self,
        profile_id: &EmbeddingProfileId,
        profile_config_hash: &str,
        entries: &[EmbeddingCacheVector<'_>],
    ) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        let now = unix_timestamp_string();
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO embedding_cache
                    (profile_id, profile_config_hash, embedding_input_hash, vector_json, vector_blob, dimension, cache_hits, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?7)
                 ON CONFLICT(profile_id, profile_config_hash, embedding_input_hash) DO UPDATE SET
                    vector_json = excluded.vector_json,
                    vector_blob = excluded.vector_blob,
                    dimension = excluded.dimension,
                    updated_at = excluded.updated_at",
            )?;
            for entry in entries {
                let vector_blob = vector_to_blob(entry.vector);
                stmt.execute(params![
                    profile_id.as_str(),
                    profile_config_hash,
                    entry.embedding_input_hash,
                    "",
                    vector_blob,
                    sql_usize(entry.vector.len()),
                    &now,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    /// Write embedding-cache rows only while their sources have not been tombstoned.
    ///
    /// The tombstone predicate is evaluated by SQLite in the same write transaction as
    /// the upsert. If deletion wins the race, no cache row is recreated; if this write
    /// wins, source deletion's transaction purges the row before committing.
    pub(crate) fn upsert_embedding_cache_vectors_for_live_sources(
        &self,
        profile_id: &EmbeddingProfileId,
        profile_config_hash: &str,
        entries: &[SourceEmbeddingCacheVector<'_>],
    ) -> Result<()> {
        if entries.is_empty() {
            return Ok(());
        }

        let now = unix_timestamp_string();
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO embedding_cache
                    (profile_id, profile_config_hash, embedding_input_hash, vector_json, vector_blob, dimension, cache_hits, created_at, updated_at)
                 SELECT ?1, ?2, ?3, ?4, ?5, ?6, 0, ?7, ?7
                 WHERE NOT EXISTS (
                    SELECT 1 FROM source_tombstones WHERE source_id = ?8
                 )
                 ON CONFLICT(profile_id, profile_config_hash, embedding_input_hash) DO UPDATE SET
                    vector_json = excluded.vector_json,
                    vector_blob = excluded.vector_blob,
                    dimension = excluded.dimension,
                    updated_at = excluded.updated_at",
            )?;
            for entry in entries {
                let vector_blob = vector_to_blob(entry.vector);
                stmt.execute(params![
                    profile_id.as_str(),
                    profile_config_hash,
                    entry.embedding_input_hash,
                    "",
                    vector_blob,
                    sql_usize(entry.vector.len()),
                    &now,
                    &entry.source_id.0,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn upsert_image_caption_attempt(
        &self,
        image_hash: &str,
        model: &str,
        prompt_version: &str,
        prompt_hash: &str,
        attempt: &CaptionAttempt,
    ) -> Result<()> {
        let caption_json = attempt
            .caption
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serialize image caption")?;
        let now = unix_timestamp_string();
        self.conn.execute(
            "INSERT INTO image_captions (image_hash, model, prompt_version, prompt_hash, status, caption_json, raw_response, error_message, attempt_count, cache_hits, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, ?10)
             ON CONFLICT(image_hash, model, prompt_hash) DO UPDATE SET
                prompt_version = excluded.prompt_version,
                status = excluded.status,
                caption_json = excluded.caption_json,
                raw_response = excluded.raw_response,
                error_message = excluded.error_message,
                attempt_count = excluded.attempt_count,
                updated_at = excluded.updated_at",
            params![
                image_hash,
                model,
                prompt_version,
                prompt_hash,
                image_caption_status_to_str(attempt.status),
                caption_json,
                attempt.raw_response.as_deref(),
                attempt.error_message.as_deref(),
                attempt.attempt_count,
                now,
            ],
        )?;
        Ok(())
    }

    /// Write an image-caption cache result only if the initiating source remains live.
    pub(crate) fn upsert_image_caption_attempt_for_live_source(
        &self,
        source_id: &SourceId,
        image_hash: &str,
        model: &str,
        prompt_version: &str,
        prompt_hash: &str,
        attempt: &CaptionAttempt,
    ) -> Result<()> {
        let caption_json = attempt
            .caption
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serialize image caption")?;
        let now = unix_timestamp_string();
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "INSERT INTO image_captions (image_hash, model, prompt_version, prompt_hash, status, caption_json, raw_response, error_message, attempt_count, cache_hits, created_at, updated_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, ?10
             WHERE NOT EXISTS (
                SELECT 1 FROM source_tombstones WHERE source_id = ?11
             )
             ON CONFLICT(image_hash, model, prompt_hash) DO UPDATE SET
                prompt_version = excluded.prompt_version,
                status = excluded.status,
                caption_json = excluded.caption_json,
                raw_response = excluded.raw_response,
                error_message = excluded.error_message,
                attempt_count = excluded.attempt_count,
                updated_at = excluded.updated_at",
            params![
                image_hash,
                model,
                prompt_version,
                prompt_hash,
                image_caption_status_to_str(attempt.status),
                caption_json,
                attempt.raw_response.as_deref(),
                attempt.error_message.as_deref(),
                attempt.attempt_count,
                now,
                &source_id.0,
            ],
        )?;
        tx.commit()?;
        Ok(())
    }
}
