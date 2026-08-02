use std::cmp::Ordering;
use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use anyhow::{bail, Context, Result};
use rusqlite::{
    params, params_from_iter,
    types::{Type, Value},
    Connection, OpenFlags, OptionalExtension, Row, Transaction,
};
use serde::de::DeserializeOwned;

use crate::collection::{
    resolve_collection_root, validate_collection_name, CollectionMember, CollectionMemberCandidate,
    CollectionRecord, CollectionRoot, CollectionRootKind, CollectionStatus, CollectionSyncReport,
};
use crate::task::{
    bounded_error, bounded_json, bounded_message, IngestTaskStage, TaskEvent, TaskId, TaskKind,
    TaskProfile, TaskProgressSnapshot, TaskSpan, TaskStatus, TaskSummary, TASK_SPAN_MAX_PER_TASK,
};
use crate::traits::VectorDocument;
use crate::types::{
    hex_sha256, Chunk, ChunkId, ChunkType, EdgeType, EmbeddingProfileId, EvidenceId, EvidenceKind,
    EvidenceUnit, GraphEdge, GraphEdgeId, GraphNode, GraphNodeId, GraphNodeKind, ImageArtifact,
    ImageId, Source, SourceEmbeddingStatus, SourceId, SourceStatus, DEFAULT_EMBEDDING_PROFILE_ID,
};
use crate::vision_caption::{ImageCaption, ImageCaptionRecord, ImageCaptionStatus};

#[path = "store_evidence_spans.rs"]
mod evidence_spans;
#[path = "source_contents_replacement.rs"]
mod source_contents_replacement;
#[path = "source_relocation.rs"]
pub(crate) mod source_relocation;
#[path = "store_cache.rs"]
mod store_cache;
#[path = "store_deletion.rs"]
mod store_deletion;
#[path = "store_statement_count.rs"]
mod store_statement_count;
pub use source_contents_replacement::{
    SourceContentsReplacement, SourceContentsReplacementReport, SourceLexicalIndexUpdate,
};
pub use source_relocation::{source_relocation_error_kind, SourceRelocationErrorKind};
pub use store_cache::SourceEmbeddingCacheVector;

#[cfg(test)]
#[path = "store_evidence_spans_tests.rs"]
mod evidence_spans_tests;

const LEGACY_EMBEDDING_PROFILE_CONFIG_HASH: &str = "legacy";
/// SQLite page cache in KB (negative value tells SQLite the number is in KB).
/// 64 MB = -65536 KB. Reduces disk I/O for the large `list_vector_documents_for_profile` scans.
const SQLITE_CACHE_SIZE_KB: i64 = -65_536;

/// Memory-mapped I/O limit: 256 MB. Lets the OS manage page eviction for the
/// multi-GB database, reducing user-space RSS pressure.
const SQLITE_MMAP_SIZE: i64 = 268_435_456;

#[path = "sqlite_durability.rs"]
mod sqlite_durability;
#[path = "store_durability.rs"]
mod store_durability;
pub use sqlite_durability::{
    is_sqlite_busy_error, map_storage_error, SqliteCheckpointMode, SqliteCheckpointStatus,
    SqliteDiskSpaceStatus, SqliteDurabilityError, SqliteDurabilityProfile, SqliteDurabilityStatus,
    SqliteEffectiveDurability, SqliteWriteOperation,
};

pub struct Store {
    conn: Connection,
    durability_profile: SqliteDurabilityProfile,
    database_path: Option<PathBuf>,
    sql_statement_counting_available: bool,
    #[cfg(test)]
    source_relocation_before_mutation_hook: std::cell::RefCell<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    source_relocation_before_parse_hook: std::cell::RefCell<Option<Box<dyn FnOnce() + Send>>>,
    #[cfg(test)]
    source_relocation_after_parse_hook: std::cell::RefCell<Option<Box<dyn FnOnce() + Send>>>,
}

/// Task list status filter used by bounded task overview queries.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskListFilter {
    /// Queued and running tasks only.
    Active,
    /// All persisted tasks, including terminal history.
    All,
}

/// Bounded task overview page plus the total number of matching tasks.
#[derive(Debug, Clone, PartialEq)]
pub struct TaskListPage {
    pub tasks: Vec<TaskSummary>,
    pub total: usize,
}

/// Bounded queue turnover counts from the most recent task event sequence window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskTurnoverWindow {
    pub event_sequence_floor: i64,
    pub event_sequence_ceiling: i64,
    pub event_limit: usize,
    pub recent_succeeded: usize,
    pub recent_failed: usize,
    pub recent_cancelled: usize,
    pub recent_backfilled: usize,
}

impl TaskTurnoverWindow {
    pub fn recent_terminalized(&self) -> usize {
        self.recent_succeeded
            .saturating_add(self.recent_failed)
            .saturating_add(self.recent_cancelled)
    }
}

/// Bounded active-task metadata counts used by task-list plateau diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskActiveMetadataAggregate {
    pub embedding_waiting: usize,
    pub oldest_embedding_wait_ms: Option<u64>,
    pub embedding_reason_buckets: Vec<TaskReasonCount>,
    pub publish_complete_running: usize,
    pub stale_reason_buckets: Vec<TaskReasonCount>,
}

/// Count for one task wait/stale reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskReasonCount {
    pub reason: String,
    pub count: usize,
}

/// Stable configuration that defines an embedding profile's vector semantics.
#[derive(Clone, Copy, Debug)]
pub struct EmbeddingProfileConfig<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub dimension: usize,
    pub normalize: bool,
    pub endpoint_identity: Option<&'a str>,
    pub requested_model: Option<&'a str>,
    pub served_model: Option<&'a str>,
    pub max_context_tokens: Option<usize>,
    pub dtype: Option<&'a str>,
    pub quantization: Option<&'a str>,
    pub weight_identity: Option<&'a str>,
    pub chunker_version: &'a str,
    pub child_target_tokens: usize,
    pub child_overlap_tokens: usize,
    pub parent_children_count: usize,
    pub embedding_input_budget_tokens: Option<usize>,
    pub query_instruction: &'a str,
    pub document_instruction: &'a str,
}

#[derive(Debug)]
pub(crate) struct StoredEmbeddingProfileConfig {
    provider: String,
    model: String,
    dimension: usize,
    normalize: bool,
    pub(crate) endpoint_identity: Option<String>,
    pub(crate) requested_model: Option<String>,
    pub(crate) served_model: Option<String>,
    pub(crate) max_context_tokens: Option<usize>,
    pub(crate) dtype: Option<String>,
    pub(crate) quantization: Option<String>,
    pub(crate) weight_identity: Option<String>,
    pub(crate) chunker_version: String,
    pub(crate) child_target_tokens: usize,
    pub(crate) child_overlap_tokens: usize,
    pub(crate) parent_children_count: usize,
    pub(crate) embedding_input_budget_tokens: Option<usize>,
    query_instruction_hash: String,
    document_instruction_hash: String,
    config_hash: String,
}

impl StoredEmbeddingProfileConfig {
    fn incompatible_config_fields(&self, next: EmbeddingProfileConfig<'_>) -> Vec<&'static str> {
        if self.config_hash == LEGACY_EMBEDDING_PROFILE_CONFIG_HASH {
            return Vec::new();
        }

        let mut fields = Vec::new();
        if self.provider != next.provider {
            fields.push("provider");
        }
        if self.model != next.model {
            fields.push("model");
        }
        if self.dimension != next.dimension {
            fields.push("dimension");
        }
        if self.normalize != next.normalize {
            fields.push("normalize");
        }
        if self.query_instruction_hash != next.query_instruction_hash() {
            fields.push("query_instruction_hash");
        }
        if self.document_instruction_hash != next.document_instruction_hash() {
            fields.push("document_instruction_hash");
        }
        fields
    }

    fn requires_vector_reset(&self, next: EmbeddingProfileConfig<'_>, next_hash: &str) -> bool {
        if self.config_hash == next_hash {
            return false;
        }
        if self.config_hash == LEGACY_EMBEDDING_PROFILE_CONFIG_HASH {
            return true;
        }
        if self.provider != next.provider
            || self.model != next.model
            || self.dimension != next.dimension
            || self.normalize != next.normalize
            || !optional_str_matches(&self.endpoint_identity, next.endpoint_identity)
            || !optional_str_matches(&self.requested_model, next.requested_model)
            || !optional_str_matches(&self.served_model, next.served_model)
            || !optional_str_matches(&self.dtype, next.dtype)
            || !optional_str_matches(&self.quantization, next.quantization)
            || !optional_str_matches(&self.weight_identity, next.weight_identity)
            || self.query_instruction_hash != next.query_instruction_hash()
            || self.document_instruction_hash != next.document_instruction_hash()
        {
            return true;
        }
        if self.chunker_version != next.chunker_version
            || self.parent_children_count != next.parent_children_count
        {
            return true;
        }
        next.child_target_tokens < self.child_target_tokens
            || next.child_overlap_tokens < self.child_overlap_tokens
            || optional_usize_decreased(
                self.embedding_input_budget_tokens,
                next.embedding_input_budget_tokens,
            )
    }

    fn preserve_unknown_capabilities<'a>(
        &'a self,
        next: EmbeddingProfileConfig<'a>,
    ) -> EmbeddingProfileConfig<'a> {
        let preserve_stored_chunking = next.max_context_tokens.is_none()
            && (self.max_context_tokens.is_some() || self.embedding_input_budget_tokens.is_some());
        EmbeddingProfileConfig {
            provider: next.provider,
            model: next.model,
            dimension: next.dimension,
            normalize: next.normalize,
            endpoint_identity: next.endpoint_identity.or(self.endpoint_identity.as_deref()),
            requested_model: next.requested_model.or(self.requested_model.as_deref()),
            served_model: next.served_model.or(self.served_model.as_deref()),
            max_context_tokens: next.max_context_tokens.or(self.max_context_tokens),
            dtype: next.dtype.or(self.dtype.as_deref()),
            quantization: next.quantization.or(self.quantization.as_deref()),
            weight_identity: next.weight_identity.or(self.weight_identity.as_deref()),
            chunker_version: next.chunker_version,
            child_target_tokens: if preserve_stored_chunking {
                self.child_target_tokens
            } else {
                next.child_target_tokens
            },
            child_overlap_tokens: if preserve_stored_chunking {
                self.child_overlap_tokens
            } else {
                next.child_overlap_tokens
            },
            parent_children_count: if preserve_stored_chunking {
                self.parent_children_count
            } else {
                next.parent_children_count
            },
            embedding_input_budget_tokens: if preserve_stored_chunking {
                self.embedding_input_budget_tokens
            } else {
                next.embedding_input_budget_tokens
            },
            query_instruction: next.query_instruction,
            document_instruction: next.document_instruction,
        }
    }
}

fn optional_str_matches(stored: &Option<String>, next: Option<&str>) -> bool {
    stored.as_deref().unwrap_or("") == next.unwrap_or("")
}

fn optional_usize_decreased(previous: Option<usize>, next: Option<usize>) -> bool {
    match (previous, next) {
        (Some(previous), Some(next)) => next < previous,
        (None, Some(_)) | (Some(_), None) | (None, None) => false,
    }
}

impl EmbeddingProfileConfig<'_> {
    pub fn query_instruction_hash(&self) -> String {
        embedding_instruction_hash(self.query_instruction)
    }

    pub fn document_instruction_hash(&self) -> String {
        embedding_instruction_hash(self.document_instruction)
    }

    pub fn config_hash(&self) -> String {
        embedding_profile_config_hash(self)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct EmbeddingCacheEntry {
    pub embedding_input_hash: String,
    pub vector: Vec<f32>,
}

/// Borrowed embedding cache payload for writing vectors already owned elsewhere.
#[derive(Debug, Clone, Copy)]
pub struct EmbeddingCacheVector<'a> {
    /// Stable hash of the prepared embedding input text and profile config.
    pub embedding_input_hash: &'a str,
    /// Dense vector payload to persist for cache reuse.
    pub vector: &'a [f32],
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EmbeddingProfileIndexGeneration {
    pub profile_id: EmbeddingProfileId,
    pub generation: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct EmbeddingProfileStorageCounts {
    pub chunk_vectors: u64,
    pub embedding_cache_entries: u64,
    pub source_embedding_statuses: u64,
    pub embeddings_meta_entries: u64,
    pub embedding_profile_index_meta_entries: u64,
    pub embedding_profiles: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VectorJsonCleanupTableStats {
    pub eligible: u64,
    pub already_clean: u64,
    pub json_only: u64,
    pub missing_blob: u64,
    pub malformed_blob: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VectorJsonCleanupTables {
    pub chunk_vectors: VectorJsonCleanupTableStats,
    pub embedding_cache: VectorJsonCleanupTableStats,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VectorJsonCleanupCleared {
    pub chunk_vectors: u64,
    pub embedding_cache: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct VectorJsonCleanupReport {
    pub tables: VectorJsonCleanupTables,
    pub cleared: VectorJsonCleanupCleared,
}

impl Store {
    /// Run related read queries against one SQLite snapshot.
    pub fn with_read_snapshot<T>(&self, operation: impl FnOnce(&Self) -> Result<T>) -> Result<T> {
        self.conn
            .execute_batch("BEGIN DEFERRED TRANSACTION")
            .context("begin read snapshot")?;
        let result = operation(self);
        match result {
            Ok(value) => {
                self.conn
                    .execute_batch("COMMIT")
                    .context("commit read snapshot")?;
                Ok(value)
            }
            Err(error) => {
                if let Err(rollback_error) = self.conn.execute_batch("ROLLBACK") {
                    return Err(error).with_context(|| {
                        format!(
                            "rollback read snapshot failed after operation error: {rollback_error}"
                        )
                    });
                }
                Err(error)
            }
        }
    }

    /// Ask SQLite to release connection-local memory it can safely discard.
    ///
    /// This is a best-effort maintenance operation; callers decide when the
    /// connection is idle enough for the brief maintenance pause.
    pub fn shrink_memory(&self) -> Result<()> {
        self.conn
            .execute_batch("PRAGMA shrink_memory;")
            .context("shrink SQLite connection memory")
    }

    pub fn vector_json_cleanup_dry_run(&self) -> Result<VectorJsonCleanupReport> {
        let chunk_vectors =
            scan_vector_json_cleanup_table(&self.conn, VectorJsonTable::ChunkVectors)?.stats;
        let embedding_cache =
            scan_vector_json_cleanup_table(&self.conn, VectorJsonTable::EmbeddingCache)?.stats;
        Ok(VectorJsonCleanupReport {
            tables: VectorJsonCleanupTables {
                chunk_vectors,
                embedding_cache,
            },
            cleared: VectorJsonCleanupCleared::default(),
        })
    }

    pub fn cleanup_vector_json_payloads(&self) -> Result<VectorJsonCleanupReport> {
        let tx = self.conn.unchecked_transaction()?;
        let chunk_vectors = scan_vector_json_cleanup_table(&tx, VectorJsonTable::ChunkVectors)?;
        let embedding_cache = scan_vector_json_cleanup_table(&tx, VectorJsonTable::EmbeddingCache)?;
        clear_vector_json_payloads(
            &tx,
            VectorJsonTable::ChunkVectors,
            &chunk_vectors.eligible_rowids,
        )?;
        clear_vector_json_payloads(
            &tx,
            VectorJsonTable::EmbeddingCache,
            &embedding_cache.eligible_rowids,
        )?;
        tx.commit()?;
        Ok(VectorJsonCleanupReport {
            tables: VectorJsonCleanupTables {
                chunk_vectors: chunk_vectors.stats,
                embedding_cache: embedding_cache.stats,
            },
            cleared: VectorJsonCleanupCleared {
                chunk_vectors: u64::try_from(chunk_vectors.eligible_rowids.len())
                    .unwrap_or(u64::MAX),
                embedding_cache: u64::try_from(embedding_cache.eligible_rowids.len())
                    .unwrap_or(u64::MAX),
            },
        })
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        self.conn.execute_batch(SCHEMA)?;
        source_relocation::ensure_unique_source_paths(&self.conn)?;
        self.conn.execute_batch(store_deletion::DELETION_SCHEMA)?;
        self.conn
            .execute_batch(store_deletion::PREVENT_RESURRECTED_SOURCES_TRIGGER)?;
        migrate_embedding_profile_tables(&self.conn)?;
        ensure_column(
            &self.conn,
            "source_tombstones",
            "images_outcome",
            "ALTER TABLE source_tombstones ADD COLUMN images_outcome TEXT NOT NULL DEFAULT 'pending'",
        )?;
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS source_tombstones_images_outcome_idx
             ON source_tombstones(images_outcome);",
        )?;
        ensure_column(
            &self.conn,
            "source_tombstones",
            "last_reconcile_attempt_ts",
            "ALTER TABLE source_tombstones ADD COLUMN last_reconcile_attempt_ts INTEGER",
        )?;
        ensure_column(
            &self.conn,
            "source_tombstones",
            "last_reconcile_attempt_seq",
            "ALTER TABLE source_tombstones ADD COLUMN last_reconcile_attempt_seq INTEGER",
        )?;
        store_deletion::ensure_reconcile_attempt_index(&self.conn)?;
        ensure_column(
            &self.conn,
            "evidence_units",
            "kind",
            "ALTER TABLE evidence_units ADD COLUMN kind TEXT NOT NULL DEFAULT 'Text'",
        )?;
        ensure_column(
            &self.conn,
            "evidence_units",
            "derived_from_evidence_id",
            "ALTER TABLE evidence_units ADD COLUMN derived_from_evidence_id TEXT",
        )?;
        ensure_column(
            &self.conn,
            "tasks",
            "progress_json",
            "ALTER TABLE tasks ADD COLUMN progress_json TEXT",
        )?;
        ensure_column(
            &self.conn,
            "tasks",
            "progress_phase",
            "ALTER TABLE tasks ADD COLUMN progress_phase TEXT",
        )?;
        ensure_column(
            &self.conn,
            "tasks",
            "progress_wait_reason",
            "ALTER TABLE tasks ADD COLUMN progress_wait_reason TEXT",
        )?;
        ensure_column(
            &self.conn,
            "tasks",
            "progress_recent_status",
            "ALTER TABLE tasks ADD COLUMN progress_recent_status TEXT",
        )?;
        ensure_column(
            &self.conn,
            "tasks",
            "progress_phase_started_at",
            "ALTER TABLE tasks ADD COLUMN progress_phase_started_at TEXT",
        )?;
        ensure_column(
            &self.conn,
            "tasks",
            "profile_json",
            "ALTER TABLE tasks ADD COLUMN profile_json TEXT",
        )?;
        self.conn.execute_batch(
            "CREATE INDEX IF NOT EXISTS tasks_active_progress_metadata_idx
                ON tasks(status, progress_phase, progress_wait_reason, progress_recent_status, progress_phase_started_at);",
        )?;
        backfill_task_progress_metadata(&self.conn)?;
        ensure_column(
            &self.conn,
            "chunks",
            "chunk_hash",
            "ALTER TABLE chunks ADD COLUMN chunk_hash TEXT NOT NULL DEFAULT ''",
        )?;
        ensure_column(
            &self.conn,
            "chunks",
            "embedding_input_hash",
            "ALTER TABLE chunks ADD COLUMN embedding_input_hash TEXT",
        )?;
        ensure_column(
            &self.conn,
            "collections",
            "watch_enabled",
            "ALTER TABLE collections ADD COLUMN watch_enabled INTEGER NOT NULL DEFAULT 0",
        )?;
        ensure_column(
            &self.conn,
            "collections",
            "auto_index_enabled",
            "ALTER TABLE collections ADD COLUMN auto_index_enabled INTEGER NOT NULL DEFAULT 1",
        )?;
        Ok(())
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }

    // --- Source ---

    pub fn add_source(&self, source: &Source) -> Result<()> {
        let inserted = self.conn.execute(
            "INSERT INTO sources (id, path, hash, status, parser_used, last_ingested_at)
             SELECT ?1, ?2, ?3, ?4, ?5, ?6
             WHERE NOT EXISTS (
                SELECT 1 FROM source_tombstones WHERE source_id = ?1
             )",
            params![
                source.id.0,
                source.path.to_str().unwrap_or(""),
                source.hash,
                status_to_str(&source.status),
                source.parser_used,
                source.last_ingested_at,
            ],
        )?;
        if inserted == 0 {
            bail!("source id is tombstoned: {}", source.id.0);
        }
        Ok(())
    }

    pub fn list_sources(&self) -> Result<Vec<Source>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, path, hash, status, parser_used, last_ingested_at FROM sources")?;
        let rows = stmt.query_map([], source_relocation::row_to_source)?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    pub fn get_source(&self, id: &SourceId) -> Result<Option<Source>> {
        self.conn
            .query_row(
                "SELECT id, path, hash, status, parser_used, last_ingested_at FROM sources WHERE id = ?1",
                params![id.0],
                source_relocation::row_to_source,
            )
            .optional()
            .map_err(Into::into)
    }

    // --- Collections ---

    pub fn create_collection(
        &self,
        name: &str,
        ignore_patterns: &[String],
    ) -> Result<CollectionRecord> {
        validate_collection_name(name)?;
        let now = unix_timestamp_string();
        let ignore_patterns_json =
            serde_json::to_string(ignore_patterns).context("serialize collection ignore rules")?;
        self.conn.execute(
            "INSERT INTO collections
                (name, ignore_patterns_json, watch_enabled, auto_index_enabled, created_at, updated_at)
             VALUES (?1, ?2, 0, 1, ?3, ?3)",
            params![name, ignore_patterns_json, now],
        )?;
        self.get_collection(name)?
            .with_context(|| format!("collection not found after create: {name}"))
    }

    pub fn list_collections(&self) -> Result<Vec<CollectionRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, ignore_patterns_json, created_at, updated_at, last_synced_at, last_sync_json, watch_enabled, auto_index_enabled
             FROM collections ORDER BY name",
        )?;
        let rows = stmt.query_map([], collection_record_from_row)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn get_collection(&self, name: &str) -> Result<Option<CollectionRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT name, ignore_patterns_json, created_at, updated_at, last_synced_at, last_sync_json, watch_enabled, auto_index_enabled
             FROM collections WHERE name = ?1",
        )?;
        stmt.query_row(params![name], collection_record_from_row)
            .optional()
            .map_err(Into::into)
    }

    pub fn update_collection_watch_settings(
        &self,
        name: &str,
        watch_enabled: bool,
        auto_index_enabled: bool,
    ) -> Result<Option<CollectionRecord>> {
        let now = unix_timestamp_string();
        let changed = self.conn.execute(
            "UPDATE collections
             SET watch_enabled = ?2, auto_index_enabled = ?3, updated_at = ?4
             WHERE name = ?1",
            params![name, watch_enabled, auto_index_enabled, now],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        self.get_collection(name)
    }

    pub fn delete_collection(&self, name: &str) -> Result<bool> {
        let deleted = self
            .conn
            .execute("DELETE FROM collections WHERE name = ?1", params![name])?;
        Ok(deleted > 0)
    }

    pub fn add_collection_root(
        &self,
        collection_name: &str,
        path: &Path,
    ) -> Result<CollectionRoot> {
        self.get_collection(collection_name)?
            .with_context(|| format!("collection not found: {collection_name}"))?;
        let (kind, canonical_path) = resolve_collection_root(path)?;
        let now = unix_timestamp_string();
        self.conn.execute(
            "INSERT INTO collection_roots
                (collection_name, path, canonical_path, kind, added_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?5)
             ON CONFLICT(collection_name, path) DO UPDATE SET
                canonical_path = excluded.canonical_path,
                kind = excluded.kind,
                updated_at = excluded.updated_at",
            params![
                collection_name,
                path.to_string_lossy(),
                canonical_path
                    .as_ref()
                    .map(|path| path.to_string_lossy().into_owned()),
                kind.as_str(),
                now,
            ],
        )?;
        self.get_collection_root(collection_name, path)?
            .with_context(|| format!("collection root not found after insert: {}", path.display()))
    }

    pub fn list_collection_roots(&self, collection_name: &str) -> Result<Vec<CollectionRoot>> {
        let mut stmt = self.conn.prepare(
            "SELECT collection_name, path, canonical_path, kind, added_at, updated_at
             FROM collection_roots WHERE collection_name = ?1 ORDER BY path",
        )?;
        let rows = stmt.query_map(params![collection_name], collection_root_from_row)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn get_collection_root(
        &self,
        collection_name: &str,
        path: &Path,
    ) -> Result<Option<CollectionRoot>> {
        let path_text = path.to_string_lossy();
        let mut stmt = self.conn.prepare(
            "SELECT collection_name, path, canonical_path, kind, added_at, updated_at
             FROM collection_roots WHERE collection_name = ?1 AND path = ?2",
        )?;
        stmt.query_row(
            params![collection_name, path_text.as_ref()],
            collection_root_from_row,
        )
        .optional()
        .map_err(Into::into)
    }

    pub fn collection_status(&self, name: &str) -> Result<Option<CollectionStatus>> {
        let Some(collection) = self.get_collection(name)? else {
            return Ok(None);
        };
        let root_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM collection_roots WHERE collection_name = ?1",
            params![name],
            |row| row.get(0),
        )?;
        let member_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM collection_members WHERE collection_name = ?1",
            params![name],
            |row| row.get(0),
        )?;
        Ok(Some(CollectionStatus {
            collection,
            root_count: root_count as usize,
            member_count: member_count as usize,
        }))
    }

    pub fn list_collection_members(&self, collection_name: &str) -> Result<Vec<CollectionMember>> {
        let mut stmt = self.conn.prepare(
            "SELECT collection_name, source_id, logical_path, source_path, updated_at
             FROM collection_members WHERE collection_name = ?1 ORDER BY logical_path",
        )?;
        let rows = stmt.query_map(params![collection_name], collection_member_from_row)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn list_collection_members_for_collections(
        &self,
        collection_names: &[String],
    ) -> Result<Vec<CollectionMember>> {
        let mut members = Vec::new();
        for name in collection_names {
            members.extend(self.list_collection_members(name)?);
        }
        Ok(members)
    }

    pub fn replace_collection_members(
        &self,
        collection_name: &str,
        candidates: &[CollectionMemberCandidate],
        mut report: CollectionSyncReport,
    ) -> Result<CollectionSyncReport> {
        self.get_collection(collection_name)?
            .with_context(|| format!("collection not found: {collection_name}"))?;
        let old_source_ids = self.collection_source_ids(collection_name)?;
        let new_source_ids = candidates
            .iter()
            .map(|candidate| candidate.source_id.0.clone())
            .collect::<std::collections::BTreeSet<_>>();
        report.member_count = candidates.len();
        report.added = new_source_ids.difference(&old_source_ids).count();
        report.removed = old_source_ids.difference(&new_source_ids).count();
        report.unchanged = old_source_ids.intersection(&new_source_ids).count();

        let now = unix_timestamp_string();
        let report_json =
            serde_json::to_string(&report).context("serialize collection sync report")?;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM collection_members WHERE collection_name = ?1",
            params![collection_name],
        )?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO collection_members
                    (collection_name, source_id, logical_path, source_path, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )?;
            for candidate in candidates {
                stmt.execute(params![
                    collection_name,
                    &candidate.source_id.0,
                    &candidate.logical_path,
                    candidate.source_path.to_string_lossy(),
                    &now,
                ])?;
            }
        }
        tx.execute(
            "UPDATE collections
             SET updated_at = ?2, last_synced_at = ?2, last_sync_json = ?3
             WHERE name = ?1",
            params![collection_name, now, report_json],
        )?;
        tx.commit()?;
        Ok(report)
    }

    fn collection_source_ids(
        &self,
        collection_name: &str,
    ) -> Result<std::collections::BTreeSet<String>> {
        let mut stmt = self.conn.prepare(
            "SELECT source_id FROM collection_members WHERE collection_name = ?1 ORDER BY source_id",
        )?;
        let rows = stmt.query_map(params![collection_name], |row| row.get::<_, String>(0))?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn replace_source_contents(
        &self,
        replacement: SourceContentsReplacement<'_>,
    ) -> Result<SourceContentsReplacementReport> {
        self.ensure_write_capacity(SqliteWriteOperation::Ingest)?;
        (|| {
            let profile_id = replacement.embedding_profile_id;
            let deleted_child_chunks =
                self.count_lexical_documents_for_source(&replacement.source.id)?;
            let tx = self.conn.unchecked_transaction()?;
            let lexical_update =
                replace_source_contents_tx(&tx, replacement, deleted_child_chunks)?;
            let _ = bump_all_profile_index_generations(&tx)?;
            let generation = profile_index_generation(&tx, profile_id)?;
            tx.commit()?;
            Ok(SourceContentsReplacementReport {
                generation,
                lexical_update,
            })
        })()
        .map_err(|error| map_storage_error(SqliteWriteOperation::Ingest, error))
    }

    pub fn replace_source_contents_without_generation(
        &self,
        replacement: SourceContentsReplacement<'_>,
    ) -> Result<SourceLexicalIndexUpdate> {
        self.ensure_write_capacity(SqliteWriteOperation::Ingest)?;
        (|| {
            let deleted_child_chunks =
                self.count_lexical_documents_for_source(&replacement.source.id)?;
            let tx = self.conn.unchecked_transaction()?;
            let lexical_update =
                replace_source_contents_tx(&tx, replacement, deleted_child_chunks)?;
            tx.commit()?;
            Ok(lexical_update)
        })()
        .map_err(|error| map_storage_error(SqliteWriteOperation::Ingest, error))
    }

    pub fn index_generation(&self) -> Result<u64> {
        self.index_generation_for_profile(&EmbeddingProfileId::default_profile())
    }

    pub fn index_generation_for_profile(&self, profile_id: &EmbeddingProfileId) -> Result<u64> {
        profile_index_generation(&self.conn, profile_id)
    }

    pub fn profile_index_generations(&self) -> Result<Vec<EmbeddingProfileIndexGeneration>> {
        let mut stmt = self.conn.prepare(
            "SELECT profile_id, generation
             FROM embedding_profile_index_meta
             ORDER BY profile_id",
        )?;
        let rows = stmt.query_map([], |row| {
            let profile_id_text: String = row.get(0)?;
            let generation_text: String = row.get(1)?;
            Ok((profile_id_text, generation_text))
        })?;

        let mut generations = Vec::new();
        for row in rows {
            let (profile_id_text, generation_text) = row?;
            let profile_id = EmbeddingProfileId::new(profile_id_text.clone())
                .map_err(|err| anyhow::anyhow!("parse profile index metadata id: {err}"))?;
            let generation = generation_text
                .parse::<u64>()
                .with_context(|| format!("parse profile index generation for {profile_id_text}"))?;
            generations.push(EmbeddingProfileIndexGeneration {
                profile_id,
                generation,
            });
        }
        Ok(generations)
    }

    pub fn embedding_profile_storage_counts(
        &self,
        profile_id: &EmbeddingProfileId,
    ) -> Result<EmbeddingProfileStorageCounts> {
        Ok(EmbeddingProfileStorageCounts {
            chunk_vectors: self.count_embedding_profile_rows(
                "SELECT COUNT(*) FROM chunk_vectors WHERE profile_id = ?1",
                profile_id,
            )?,
            embedding_cache_entries: self.count_embedding_profile_rows(
                "SELECT COUNT(*) FROM embedding_cache WHERE profile_id = ?1",
                profile_id,
            )?,
            source_embedding_statuses: self.count_embedding_profile_rows(
                "SELECT COUNT(*) FROM source_embedding_status WHERE profile_id = ?1",
                profile_id,
            )?,
            embeddings_meta_entries: self.count_embedding_profile_rows(
                "SELECT COUNT(*) FROM embeddings_meta WHERE profile_id = ?1",
                profile_id,
            )?,
            embedding_profile_index_meta_entries: self.count_embedding_profile_rows(
                "SELECT COUNT(*) FROM embedding_profile_index_meta WHERE profile_id = ?1",
                profile_id,
            )?,
            embedding_profiles: self.count_embedding_profile_rows(
                "SELECT COUNT(*) FROM embedding_profiles WHERE id = ?1",
                profile_id,
            )?,
        })
    }

    pub fn delete_embedding_profile_index_data(
        &self,
        profile_id: &EmbeddingProfileId,
    ) -> Result<EmbeddingProfileStorageCounts> {
        let tx = self.conn.unchecked_transaction()?;
        let counts = EmbeddingProfileStorageCounts {
            chunk_vectors: tx
                .execute(
                    "DELETE FROM chunk_vectors WHERE profile_id = ?1",
                    params![profile_id.as_str()],
                )?
                .try_into()
                .unwrap_or_default(),
            embedding_cache_entries: tx
                .execute(
                    "DELETE FROM embedding_cache WHERE profile_id = ?1",
                    params![profile_id.as_str()],
                )?
                .try_into()
                .unwrap_or_default(),
            source_embedding_statuses: tx
                .execute(
                    "DELETE FROM source_embedding_status WHERE profile_id = ?1",
                    params![profile_id.as_str()],
                )?
                .try_into()
                .unwrap_or_default(),
            embeddings_meta_entries: tx
                .execute(
                    "DELETE FROM embeddings_meta WHERE profile_id = ?1",
                    params![profile_id.as_str()],
                )?
                .try_into()
                .unwrap_or_default(),
            embedding_profile_index_meta_entries: tx
                .execute(
                    "DELETE FROM embedding_profile_index_meta WHERE profile_id = ?1",
                    params![profile_id.as_str()],
                )?
                .try_into()
                .unwrap_or_default(),
            embedding_profiles: tx
                .execute(
                    "DELETE FROM embedding_profiles WHERE id = ?1",
                    params![profile_id.as_str()],
                )?
                .try_into()
                .unwrap_or_default(),
        };
        tx.commit()?;
        Ok(counts)
    }

    fn count_embedding_profile_rows(
        &self,
        query: &str,
        profile_id: &EmbeddingProfileId,
    ) -> Result<u64> {
        let count: i64 = self
            .conn
            .query_row(query, params![profile_id.as_str()], |row| row.get(0))?;
        Ok(count.try_into().unwrap_or_default())
    }

    pub fn update_source_status(&self, id: &SourceId, status: &SourceStatus) -> Result<()> {
        self.conn.execute(
            "UPDATE sources SET status = ?1 WHERE id = ?2",
            params![status_to_str(status), id.0],
        )?;
        Ok(())
    }

    pub fn update_source_hash(&self, id: &SourceId, hash: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE sources SET hash = ?1 WHERE id = ?2",
            params![hash, id.0],
        )?;
        Ok(())
    }

    pub fn find_stale_sources(
        &self,
        current_hashes: &HashMap<SourceId, String>,
    ) -> Result<Vec<SourceId>> {
        let sources = self.list_sources()?;
        let stale: Vec<SourceId> = sources
            .into_iter()
            .filter(|s| {
                current_hashes
                    .get(&s.id)
                    .is_some_and(|current| *current != s.hash)
            })
            .map(|s| s.id)
            .collect();
        Ok(stale)
    }

    pub fn find_stale_sources_for_lexical_index(
        &self,
        current_hashes: &HashMap<SourceId, String>,
    ) -> Result<Vec<SourceId>> {
        let sources = self.list_sources()?;
        let stale: Vec<SourceId> = sources
            .into_iter()
            .filter(|source| {
                let file_stale = current_hashes
                    .get(&source.id)
                    .is_some_and(|current| *current != source.hash);
                let source_status_stale = source.status != SourceStatus::Indexed;
                file_stale || source_status_stale
            })
            .map(|source| source.id)
            .collect();
        Ok(stale)
    }

    pub fn find_stale_sources_for_profile(
        &self,
        current_hashes: &HashMap<SourceId, String>,
        profile_id: &EmbeddingProfileId,
    ) -> Result<Vec<SourceId>> {
        let sources = self.list_sources()?;
        let mut stale = Vec::new();
        for source in sources {
            let file_stale = current_hashes
                .get(&source.id)
                .is_some_and(|current| *current != source.hash);
            let source_status_stale = source.status != SourceStatus::Indexed;
            let vectors_stale = self.source_vectors_stale_for_profile(profile_id, &source.id)?;
            if file_stale || source_status_stale || vectors_stale {
                stale.push(source.id);
            }
        }
        Ok(stale)
    }

    /// Return whether a source is missing fresh child vectors for an embedding profile.
    pub fn source_vectors_stale_for_profile(
        &self,
        profile_id: &EmbeddingProfileId,
        source_id: &SourceId,
    ) -> Result<bool> {
        let child_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM chunks WHERE source_id = ?1 AND chunk_type = ?2",
            params![&source_id.0, chunk_type_to_str(&ChunkType::Child)],
            |row| row.get(0),
        )?;
        let vector_count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM chunk_vectors WHERE profile_id = ?1 AND source_id = ?2",
            params![profile_id.as_str(), &source_id.0],
            |row| row.get(0),
        )?;
        let status: Option<(String, i64)> = self
            .conn
            .query_row(
                "SELECT status, vector_count
                 FROM source_embedding_status
                 WHERE profile_id = ?1 AND source_id = ?2",
                params![profile_id.as_str(), &source_id.0],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .optional()?;
        let Some((status, recorded_vector_count)) = status else {
            return Ok(true);
        };
        Ok(status != SourceEmbeddingStatus::Embedded.as_str()
            || recorded_vector_count != child_count
            || vector_count != child_count)
    }

    // --- EvidenceUnit ---

    pub fn bulk_insert_evidence(&self, units: &[EvidenceUnit]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        insert_evidence_units_tx(&tx, units)?;
        tx.commit()?;
        Ok(())
    }

    pub fn get_evidence(&self, id: &EvidenceId) -> Result<Option<EvidenceUnit>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, kind, locator_json, text, text_hash, heading_path_json, position, derived_from_evidence_id FROM evidence_units WHERE id = ?1"
        )?;
        let mut rows = stmt.query_map(params![id.0], row_to_evidence_unit)?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn list_evidence_by_source(&self, source_id: &SourceId) -> Result<Vec<EvidenceUnit>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, kind, locator_json, text, text_hash, heading_path_json, position, derived_from_evidence_id FROM evidence_units WHERE source_id = ?1 ORDER BY position"
        )?;
        let rows = stmt.query_map(params![source_id.0], row_to_evidence_unit)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    // --- ImageArtifact ---

    pub fn bulk_insert_image_artifacts(&self, artifacts: &[ImageArtifact]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        insert_image_artifacts_tx(&tx, artifacts)?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_image_artifacts_by_source(
        &self,
        source_id: &SourceId,
    ) -> Result<Vec<ImageArtifact>> {
        let mut stmt = self.conn.prepare(
            "SELECT image_id, source_id, evidence_unit_id, relative_path, content_hash, mime_type, width, height, page, image_index, bbox_json FROM image_artifacts WHERE source_id = ?1 ORDER BY page, image_index"
        )?;
        let rows = stmt.query_map(params![source_id.0], row_to_image_artifact)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn get_image_artifact_by_evidence(
        &self,
        evidence_id: &EvidenceId,
    ) -> Result<Option<ImageArtifact>> {
        let mut stmt = self.conn.prepare(
            "SELECT image_id, source_id, evidence_unit_id, relative_path, content_hash, mime_type, width, height, page, image_index, bbox_json FROM image_artifacts WHERE evidence_unit_id = ?1"
        )?;
        let mut rows = stmt.query_map(params![evidence_id.0], row_to_image_artifact)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn get_image_artifact(&self, image_id: &ImageId) -> Result<Option<ImageArtifact>> {
        let mut stmt = self.conn.prepare(
            "SELECT image_id, source_id, evidence_unit_id, relative_path, content_hash, mime_type, width, height, page, image_index, bbox_json FROM image_artifacts WHERE image_id = ?1"
        )?;
        let mut rows = stmt.query_map(params![image_id.0], row_to_image_artifact)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    // --- ImageCaption ---

    pub fn get_image_caption(
        &self,
        image_hash: &str,
        model: &str,
        prompt_hash: &str,
    ) -> Result<Option<ImageCaptionRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT image_hash, model, prompt_version, prompt_hash, status, caption_json, raw_response, error_message, attempt_count, cache_hits, created_at, updated_at FROM image_captions WHERE image_hash = ?1 AND model = ?2 AND prompt_hash = ?3"
        )?;
        stmt.query_row(
            params![image_hash, model, prompt_hash],
            row_to_image_caption_record,
        )
        .optional()
        .map_err(Into::into)
    }

    pub(crate) fn get_successful_image_caption(
        &self,
        image_hash: &str,
        model: &str,
        prompt_hash: &str,
    ) -> Result<Option<ImageCaptionRecord>> {
        let record = self.get_image_caption(image_hash, model, prompt_hash)?;
        Ok(record.filter(|record| {
            record.status == ImageCaptionStatus::Success && record.caption.is_some()
        }))
    }

    pub fn list_image_captions(&self) -> Result<Vec<ImageCaptionRecord>> {
        let mut stmt = self.conn.prepare(
            "SELECT image_hash, model, prompt_version, prompt_hash, status, caption_json, raw_response, error_message, attempt_count, cache_hits, created_at, updated_at FROM image_captions ORDER BY image_hash, model, prompt_hash"
        )?;
        let rows = stmt.query_map([], row_to_image_caption_record)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub(crate) fn record_image_caption_cache_hit(
        &self,
        image_hash: &str,
        model: &str,
        prompt_hash: &str,
    ) -> Result<()> {
        let now = unix_timestamp_string();
        self.conn.execute(
            "UPDATE image_captions SET cache_hits = cache_hits + 1, updated_at = ?4 WHERE image_hash = ?1 AND model = ?2 AND prompt_hash = ?3",
            params![image_hash, model, prompt_hash, now],
        )?;
        Ok(())
    }

    // --- Chunk ---

    pub fn bulk_insert_chunks(&self, chunks: &[Chunk]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT INTO chunks (id, source_id, chunk_hash, embedding_input_hash, text, context_text, token_count, chunk_type, parent_chunk_id, heading_path_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
            )?;
            for c in chunks {
                let heading_json =
                    serde_json::to_string(&c.heading_path).context("serialize heading_path")?;
                stmt.execute(params![
                    &c.id.0,
                    &c.source_id.0,
                    &c.chunk_hash,
                    &c.embedding_input_hash,
                    &c.text,
                    &c.context_text,
                    c.token_count,
                    chunk_type_to_str(&c.chunk_type),
                    c.parent_chunk_id.as_ref().map(|id| &id.0),
                    heading_json,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn get_chunk(&self, id: &ChunkId) -> Result<Option<Chunk>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, chunk_hash, embedding_input_hash, text, context_text, token_count, chunk_type, parent_chunk_id, heading_path_json FROM chunks WHERE id = ?1"
        )?;
        let mut rows = stmt.query_map(params![id.0], row_to_chunk_tuple)?;
        match rows.next() {
            Some(r) => {
                let t = r?;
                Ok(Some(tuple_to_chunk(t, &self.conn)?))
            }
            None => Ok(None),
        }
    }

    pub fn list_chunks_by_source(&self, source_id: &SourceId) -> Result<Vec<Chunk>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, chunk_hash, embedding_input_hash, text, context_text, token_count, chunk_type, parent_chunk_id, heading_path_json FROM chunks WHERE source_id = ?1"
        )?;
        let rows = stmt.query_map(params![source_id.0], row_to_chunk_tuple)?;
        let mut result = Vec::new();
        for r in rows {
            result.push(tuple_to_chunk(r?, &self.conn)?);
        }
        Ok(result)
    }

    pub fn list_child_chunks(&self) -> Result<Vec<Chunk>> {
        let mut stmt = self.conn.prepare(
        "SELECT id, source_id, chunk_hash, embedding_input_hash, text, context_text, token_count, chunk_type, parent_chunk_id, heading_path_json FROM chunks WHERE chunk_type = 'Child' ORDER BY source_id, id"
        )?;
        let rows = stmt.query_map([], row_to_chunk_tuple)?;
        let mut result = Vec::new();
        for r in rows {
            result.push(tuple_to_chunk(r?, &self.conn)?);
        }
        Ok(result)
    }

    pub fn get_parent_chunk(&self, child_id: &ChunkId) -> Result<Option<Chunk>> {
        let child = self.get_chunk(child_id)?;
        match child.and_then(|c| c.parent_chunk_id) {
            Some(pid) => self.get_chunk(&pid),
            None => Ok(None),
        }
    }

    // --- ChunkEvidence ---

    pub fn link_chunk_evidence(&self, links: &[(ChunkId, EvidenceId)]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(
                "INSERT OR IGNORE INTO chunk_evidence (chunk_id, evidence_unit_id) VALUES (?1, ?2)",
            )?;
            for (cid, eid) in links {
                stmt.execute(params![cid.0, eid.0])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn list_chunks_for_evidence(&self, evidence_id: &EvidenceId) -> Result<Vec<Chunk>> {
        let mut stmt = self.conn.prepare(
            "SELECT c.id, c.source_id, c.chunk_hash, c.embedding_input_hash, c.text, c.context_text, c.token_count, c.chunk_type, c.parent_chunk_id, c.heading_path_json
             FROM chunks c
             INNER JOIN chunk_evidence ce ON ce.chunk_id = c.id
             WHERE ce.evidence_unit_id = ?1
             ORDER BY CASE c.chunk_type WHEN 'Child' THEN 0 ELSE 1 END, c.id",
        )?;
        let rows = stmt.query_map(params![evidence_id.0], row_to_chunk_tuple)?;
        let mut result = Vec::new();
        for row in rows {
            result.push(tuple_to_chunk(row?, &self.conn)?);
        }
        Ok(result)
    }

    // --- EvidenceGraph ---

    pub fn get_graph_node(&self, node_id: &GraphNodeId) -> Result<Option<GraphNode>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, kind, external_id, label, locator_json, ordinal, metadata_json
             FROM graph_nodes
             WHERE id = ?1",
        )?;
        let mut rows = stmt.query_map(params![node_id.0], row_to_graph_node)?;
        match rows.next() {
            Some(row) => Ok(Some(row?)),
            None => Ok(None),
        }
    }

    pub fn upsert_graph_nodes(&self, nodes: &[GraphNode]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        upsert_graph_nodes_tx(&tx, nodes)?;
        tx.commit()?;
        Ok(())
    }

    pub fn upsert_graph_edges(&self, edges: &[GraphEdge]) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        upsert_graph_edges_tx(&tx, edges)?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_graph_nodes_by_source(&self, source_id: &SourceId) -> Result<Vec<GraphNode>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, kind, external_id, label, locator_json, ordinal, metadata_json
             FROM graph_nodes
             WHERE source_id = ?1
             ORDER BY kind, ordinal, id",
        )?;
        let rows = stmt.query_map(params![source_id.0], row_to_graph_node)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn list_graph_nodes(&self) -> Result<Vec<GraphNode>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, kind, external_id, label, locator_json, ordinal, metadata_json
             FROM graph_nodes
             ORDER BY source_id, kind, ordinal, id",
        )?;
        let rows = stmt.query_map([], row_to_graph_node)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn list_graph_edges_by_source(&self, source_id: &SourceId) -> Result<Vec<GraphEdge>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, edge_type, from_node_id, to_node_id, ordinal, weight, metadata_json
             FROM graph_edges
             WHERE source_id = ?1
             ORDER BY edge_type, ordinal, id",
        )?;
        let rows = stmt.query_map(params![source_id.0], row_to_graph_edge)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn list_graph_edges(&self) -> Result<Vec<GraphEdge>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, edge_type, from_node_id, to_node_id, ordinal, weight, metadata_json
             FROM graph_edges
             ORDER BY source_id, edge_type, ordinal, id",
        )?;
        let rows = stmt.query_map([], row_to_graph_edge)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn list_graph_edges_from(&self, node_id: &GraphNodeId) -> Result<Vec<GraphEdge>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, edge_type, from_node_id, to_node_id, ordinal, weight, metadata_json
             FROM graph_edges
             WHERE from_node_id = ?1
             ORDER BY edge_type, ordinal, id",
        )?;
        let rows = stmt.query_map(params![node_id.0], row_to_graph_edge)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn list_graph_edges_from_by_types_limited(
        &self,
        node_id: &GraphNodeId,
        edge_types: &[EdgeType],
        limit: usize,
    ) -> Result<Vec<GraphEdge>> {
        self.list_graph_edges_by_endpoint_types_limited("from_node_id", node_id, edge_types, limit)
    }

    pub fn list_graph_edges_to(&self, node_id: &GraphNodeId) -> Result<Vec<GraphEdge>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, edge_type, from_node_id, to_node_id, ordinal, weight, metadata_json
             FROM graph_edges
             WHERE to_node_id = ?1
             ORDER BY edge_type, ordinal, id",
        )?;
        let rows = stmt.query_map(params![node_id.0], row_to_graph_edge)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn list_graph_edges_to_by_types_limited(
        &self,
        node_id: &GraphNodeId,
        edge_types: &[EdgeType],
        limit: usize,
    ) -> Result<Vec<GraphEdge>> {
        self.list_graph_edges_by_endpoint_types_limited("to_node_id", node_id, edge_types, limit)
    }

    pub fn list_graph_edges_by_type(
        &self,
        source_id: &SourceId,
        edge_type: EdgeType,
    ) -> Result<Vec<GraphEdge>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, source_id, edge_type, from_node_id, to_node_id, ordinal, weight, metadata_json
             FROM graph_edges
             WHERE source_id = ?1 AND edge_type = ?2
             ORDER BY ordinal, id",
        )?;
        let rows = stmt.query_map(params![source_id.0, edge_type.as_str()], row_to_graph_edge)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    fn list_graph_edges_by_endpoint_types_limited(
        &self,
        endpoint_column: &str,
        node_id: &GraphNodeId,
        edge_types: &[EdgeType],
        limit: usize,
    ) -> Result<Vec<GraphEdge>> {
        if edge_types.is_empty() || limit == 0 {
            return Ok(Vec::new());
        }

        let placeholders = std::iter::repeat_n("?", edge_types.len())
            .collect::<Vec<_>>()
            .join(", ");
        let sql = format!(
            "SELECT id, source_id, edge_type, from_node_id, to_node_id, ordinal, weight, metadata_json
             FROM graph_edges
             WHERE {endpoint_column} = ? AND edge_type IN ({placeholders})
             ORDER BY edge_type, ordinal, id
             LIMIT ?",
        );
        let mut values = Vec::with_capacity(edge_types.len() + 2);
        values.push(Value::Text(node_id.0.clone()));
        values.extend(
            edge_types
                .iter()
                .map(|edge_type| Value::Text(edge_type.as_str().to_string())),
        );
        values.push(Value::Integer(sql_limit(limit)));

        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(values), row_to_graph_edge)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn remove_graph_by_source(&self, source_id: &SourceId) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute(
            "DELETE FROM graph_edges WHERE source_id = ?1",
            params![&source_id.0],
        )?;
        tx.execute(
            "DELETE FROM graph_nodes WHERE source_id = ?1",
            params![&source_id.0],
        )?;
        tx.commit()?;
        Ok(())
    }

    // --- EmbeddingsMeta ---

    pub fn ensure_embedding_profile(
        &self,
        profile_id: &EmbeddingProfileId,
        config: EmbeddingProfileConfig<'_>,
    ) -> Result<bool> {
        let now = unix_timestamp_string();
        let query_instruction_hash = config.query_instruction_hash();
        let document_instruction_hash = config.document_instruction_hash();
        let config_hash = config.config_hash();
        self.conn.execute(
            "INSERT INTO embedding_profiles
                (id, provider, model, dimension, normalize, endpoint_identity, requested_model, served_model, max_context_tokens, dtype, quantization, weight_identity, chunker_version, child_target_tokens, child_overlap_tokens, parent_children_count, embedding_input_budget_tokens, query_instruction_hash, document_instruction_hash, config_hash, qdrant_collection, qdrant_vector_name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, NULL, NULL, ?21, ?21)
             ON CONFLICT(id) DO NOTHING",
            params![
                profile_id.as_str(),
                config.provider,
                config.model,
                sql_usize(config.dimension),
                config.normalize,
                config.endpoint_identity,
                config.requested_model,
                config.served_model,
                sql_opt_usize(config.max_context_tokens),
                config.dtype,
                config.quantization,
                config.weight_identity,
                config.chunker_version,
                sql_usize(config.child_target_tokens),
                sql_usize(config.child_overlap_tokens),
                sql_usize(config.parent_children_count),
                sql_opt_usize(config.embedding_input_budget_tokens),
                &query_instruction_hash,
                &document_instruction_hash,
                &config_hash,
                &now,
            ],
        )?;
        self.conn.execute(
            "INSERT OR IGNORE INTO embedding_profile_index_meta (profile_id, generation)
             VALUES (?1, '0')",
            params![profile_id.as_str()],
        )?;

        let Some(existing) = self.load_embedding_profile_config(profile_id)? else {
            bail!("embedding profile was not persisted: {profile_id}");
        };
        let effective_config = existing.preserve_unknown_capabilities(config);
        let query_instruction_hash = effective_config.query_instruction_hash();
        let document_instruction_hash = effective_config.document_instruction_hash();
        let config_hash = effective_config.config_hash();
        if existing.config_hash != config_hash {
            let incompatible_fields = existing.incompatible_config_fields(effective_config);
            if !incompatible_fields.is_empty() {
                bail!(
                    "embedding profile '{}' already exists with incompatible config fields: {}",
                    profile_id.as_str(),
                    incompatible_fields.join(", ")
                );
            }
            let reset_vectors = existing.requires_vector_reset(effective_config, &config_hash);
            let tx = self.conn.unchecked_transaction()?;
            update_embedding_profile_config_tx(
                &tx,
                profile_id,
                effective_config,
                &query_instruction_hash,
                &document_instruction_hash,
                &config_hash,
            )?;
            if reset_vectors {
                clear_profile_vector_state_tx(&tx, profile_id)?;
            }
            tx.commit()?;
            return Ok(reset_vectors);
        }

        Ok(false)
    }

    pub(crate) fn load_embedding_profile_config(
        &self,
        profile_id: &EmbeddingProfileId,
    ) -> Result<Option<StoredEmbeddingProfileConfig>> {
        self.conn
            .query_row(
                "SELECT provider, model, dimension, normalize, endpoint_identity, requested_model,
                        served_model, max_context_tokens, dtype, quantization, weight_identity,
                        chunker_version, child_target_tokens, child_overlap_tokens,
                        parent_children_count, embedding_input_budget_tokens, query_instruction_hash,
                        document_instruction_hash, config_hash
                 FROM embedding_profiles
                 WHERE id = ?1",
                params![profile_id.as_str()],
                |row| {
                    Ok(StoredEmbeddingProfileConfig {
                        provider: row.get(0)?,
                        model: row.get(1)?,
                        dimension: row_usize(row, 2)?,
                        normalize: row.get(3)?,
                        endpoint_identity: row.get(4)?,
                        requested_model: row.get(5)?,
                        served_model: row.get(6)?,
                        max_context_tokens: row_opt_usize(row, 7)?,
                        dtype: row.get(8)?,
                        quantization: row.get(9)?,
                        weight_identity: row.get(10)?,
                        chunker_version: row.get(11)?,
                        child_target_tokens: row_usize(row, 12)?,
                        child_overlap_tokens: row_usize(row, 13)?,
                        parent_children_count: row_usize(row, 14)?,
                        embedding_input_budget_tokens: row_opt_usize(row, 15)?,
                        query_instruction_hash: row.get(16)?,
                        document_instruction_hash: row.get(17)?,
                        config_hash: row.get(18)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn set_embedding_meta(
        &self,
        chunk_id: &ChunkId,
        hnsw_position: i64,
        embedding_model: &str,
        embedded_at: &str,
    ) -> Result<()> {
        self.set_embedding_meta_for_profile(
            &EmbeddingProfileId::default_profile(),
            chunk_id,
            hnsw_position,
            embedding_model,
            embedded_at,
        )
    }

    pub fn set_embedding_meta_for_profile(
        &self,
        profile_id: &EmbeddingProfileId,
        chunk_id: &ChunkId,
        hnsw_position: i64,
        embedding_model: &str,
        embedded_at: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO embeddings_meta
                (profile_id, chunk_id, hnsw_position, embedding_model, embedded_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                profile_id.as_str(),
                chunk_id.0,
                hnsw_position,
                embedding_model,
                embedded_at
            ],
        )?;
        Ok(())
    }

    pub fn get_embedding_meta(&self, chunk_id: &ChunkId) -> Result<Option<(i64, String, String)>> {
        self.get_embedding_meta_for_profile(&EmbeddingProfileId::default_profile(), chunk_id)
    }

    pub fn get_embedding_meta_for_profile(
        &self,
        profile_id: &EmbeddingProfileId,
        chunk_id: &ChunkId,
    ) -> Result<Option<(i64, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT hnsw_position, embedding_model, embedded_at
             FROM embeddings_meta
             WHERE profile_id = ?1 AND chunk_id = ?2",
        )?;
        let mut rows = stmt.query_map(params![profile_id.as_str(), chunk_id.0], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn replace_all_vector_documents(&self, vectors: &[VectorDocument]) -> Result<()> {
        self.replace_all_vector_documents_for_profile(
            &EmbeddingProfileId::default_profile(),
            vectors,
        )
    }

    pub fn replace_all_vector_documents_for_profile(
        &self,
        profile_id: &EmbeddingProfileId,
        vectors: &[VectorDocument],
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        replace_vector_documents_for_profile_tx(&tx, profile_id, vectors)?;
        let _ = bump_profile_index_generation(&tx, profile_id)?;
        tx.commit()?;
        Ok(())
    }

    pub fn replace_source_vector_documents_for_profile(
        &self,
        profile_id: &EmbeddingProfileId,
        source_id: &SourceId,
        vectors: &[VectorDocument],
    ) -> Result<u64> {
        let tx = self.conn.unchecked_transaction()?;
        replace_source_vector_documents_for_profile_tx(&tx, profile_id, source_id, vectors)?;
        set_source_embedding_status_tx(
            &tx,
            profile_id,
            source_id,
            SourceEmbeddingStatus::Embedded,
            vectors.len(),
            None,
        )?;
        let generation = bump_profile_index_generation(&tx, profile_id)?;
        tx.commit()?;
        Ok(generation)
    }

    pub fn list_vector_documents(&self) -> Result<Vec<VectorDocument>> {
        self.list_vector_documents_for_profile(&EmbeddingProfileId::default_profile())
    }

    pub fn list_vector_documents_for_profile(
        &self,
        profile_id: &EmbeddingProfileId,
    ) -> Result<Vec<VectorDocument>> {
        let mut stmt = self.conn.prepare(
            "SELECT chunk_id, source_id, vector_blob, vector_json
             FROM chunk_vectors
             WHERE profile_id = ?1
             ORDER BY source_id, chunk_id",
        )?;
        let rows = stmt.query_map(params![profile_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<Vec<u8>>>(2)?,
                row.get::<_, Option<String>>(3)?,
            ))
        })?;

        let mut result = Vec::new();
        for row in rows {
            let (chunk_id, source_id, vector_blob, vector_json) = row?;
            let vector = decode_stored_vector(
                &format!("chunk {chunk_id}"),
                vector_blob.as_deref(),
                vector_json.as_deref(),
            )?;
            result.push(VectorDocument {
                chunk_id: ChunkId(chunk_id),
                source_id: SourceId(source_id),
                vector,
            });
        }
        Ok(result)
    }

    pub fn has_vector_document_for_profile(
        &self,
        profile_id: &EmbeddingProfileId,
        chunk_id: &ChunkId,
        source_id: &SourceId,
    ) -> Result<bool> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*)
             FROM chunk_vectors
             WHERE profile_id = ?1 AND chunk_id = ?2 AND source_id = ?3",
            params![profile_id.as_str(), &chunk_id.0, &source_id.0],
            |row| row.get(0),
        )?;
        Ok(count > 0)
    }

    /// Search SQLite-stored vectors for one profile without building a resident vector index.
    pub fn search_vector_documents_for_profile(
        &self,
        profile_id: &EmbeddingProfileId,
        query: &[f32],
        top_k: usize,
        source_filter: Option<&HashSet<SourceId>>,
    ) -> Result<Vec<(ChunkId, f32)>> {
        if top_k == 0 || query.is_empty() {
            return Ok(Vec::new());
        }
        if source_filter.is_some_and(HashSet::is_empty) {
            return Ok(Vec::new());
        }

        let mut hits = Vec::new();
        let sorted_source_ids = source_filter.map(sorted_source_filter_ids);
        self.search_vector_blob_documents_for_profile(
            profile_id,
            query,
            top_k,
            sorted_source_ids.as_deref(),
            &mut hits,
        )?;
        self.search_vector_json_documents_for_profile(
            profile_id,
            query,
            top_k,
            sorted_source_ids.as_deref(),
            &mut hits,
        )?;

        hits.sort_by(|left, right| {
            right
                .1
                .partial_cmp(&left.1)
                .unwrap_or(Ordering::Equal)
                .then_with(|| left.0 .0.cmp(&right.0 .0))
        });
        Ok(hits)
    }

    fn search_vector_blob_documents_for_profile(
        &self,
        profile_id: &EmbeddingProfileId,
        query: &[f32],
        top_k: usize,
        source_ids: Option<&[String]>,
        hits: &mut Vec<(ChunkId, f32)>,
    ) -> Result<()> {
        let source_clause = vector_scan_source_clause(source_ids);
        let sql = format!(
            "SELECT chunk_id, vector_blob, vector_json
             FROM chunk_vectors
             WHERE profile_id = ?
               AND vector_blob IS NOT NULL
               AND length(vector_blob) > 0{source_clause}
             ORDER BY source_id, chunk_id"
        );
        let params = vector_scan_params(profile_id, source_ids);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params.iter()), |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Vec<u8>>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })?;
        for row in rows {
            let (chunk_id, vector_blob, vector_json) = row?;
            let score = match vector_blob_search_score(query, &vector_blob) {
                Ok(score) => score,
                Err(blob_error) => {
                    let json = non_empty_vector_json(vector_json.as_deref()).ok_or_else(|| {
                        blob_error.context(format!(
                            "score vector BLOB for chunk {chunk_id}; no JSON fallback is present"
                        ))
                    })?;
                    let vector = parse_stored_vector(&format!("chunk {chunk_id}"), json)
                        .with_context(|| {
                            format!("parse JSON fallback after malformed BLOB for chunk {chunk_id}")
                        })?;
                    vector_search_score(query, &vector)
                }
            };
            push_top_vector_hit(hits, top_k, ChunkId(chunk_id), score);
        }
        Ok(())
    }

    fn search_vector_json_documents_for_profile(
        &self,
        profile_id: &EmbeddingProfileId,
        query: &[f32],
        top_k: usize,
        source_ids: Option<&[String]>,
        hits: &mut Vec<(ChunkId, f32)>,
    ) -> Result<()> {
        let source_clause = vector_scan_source_clause(source_ids);
        let sql = format!(
            "SELECT chunk_id, vector_json
             FROM chunk_vectors
             WHERE profile_id = ?
               AND (vector_blob IS NULL OR length(vector_blob) = 0){source_clause}
             ORDER BY source_id, chunk_id"
        );
        let params = vector_scan_params(profile_id, source_ids);
        let mut stmt = self.conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(params.iter()), |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })?;
        for row in rows {
            let (chunk_id, vector_json) = row?;
            let json = non_empty_vector_json(vector_json.as_deref())
                .ok_or_else(|| anyhow::anyhow!("missing vector for chunk {chunk_id}"))?;
            let vector = parse_stored_vector(&format!("chunk {chunk_id}"), json)?;
            push_top_vector_hit(
                hits,
                top_k,
                ChunkId(chunk_id),
                vector_search_score(query, &vector),
            );
        }
        Ok(())
    }

    pub fn count_vector_documents_for_profile(
        &self,
        profile_id: &EmbeddingProfileId,
        source_filter: Option<&SourceId>,
    ) -> Result<usize> {
        let count: i64 = match source_filter {
            Some(source_id) => self.conn.query_row(
                "SELECT COUNT(*) FROM chunk_vectors WHERE profile_id = ?1 AND source_id = ?2",
                params![profile_id.as_str(), source_id.0],
                |row| row.get(0),
            )?,
            None => self.conn.query_row(
                "SELECT COUNT(*) FROM chunk_vectors WHERE profile_id = ?1",
                params![profile_id.as_str()],
                |row| row.get(0),
            )?,
        };
        Ok(count.try_into().unwrap_or_default())
    }

    pub fn count_lexical_documents_for_source(&self, source_id: &SourceId) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM chunk_fts WHERE source_id = ?1",
            params![&source_id.0],
            |row| row.get(0),
        )?;
        Ok(count.try_into().unwrap_or_default())
    }

    pub fn get_embedding_cache_vector(
        &self,
        profile_id: &EmbeddingProfileId,
        profile_config_hash: &str,
        embedding_input_hash: &str,
    ) -> Result<Option<Vec<f32>>> {
        let row = self
            .conn
            .query_row(
                "SELECT vector_blob, vector_json
                 FROM embedding_cache
                 WHERE profile_id = ?1
                   AND profile_config_hash = ?2
                   AND embedding_input_hash = ?3",
                params![
                    profile_id.as_str(),
                    profile_config_hash,
                    embedding_input_hash,
                ],
                |row| {
                    Ok((
                        row.get::<_, Option<Vec<u8>>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .optional()?;
        row.map(|(blob, json)| {
            decode_stored_vector(
                &format!("embedding cache input {embedding_input_hash}"),
                blob.as_deref(),
                json.as_deref(),
            )
        })
        .transpose()
    }

    pub fn record_embedding_cache_hit(
        &self,
        profile_id: &EmbeddingProfileId,
        profile_config_hash: &str,
        embedding_input_hash: &str,
    ) -> Result<()> {
        let now = unix_timestamp_string();
        self.conn.execute(
            "UPDATE embedding_cache
             SET cache_hits = cache_hits + 1, updated_at = ?4
             WHERE profile_id = ?1
               AND profile_config_hash = ?2
               AND embedding_input_hash = ?3",
            params![
                profile_id.as_str(),
                profile_config_hash,
                embedding_input_hash,
                now,
            ],
        )?;
        Ok(())
    }

    pub fn set_source_embedding_status(
        &self,
        profile_id: &EmbeddingProfileId,
        source_id: &SourceId,
        status: SourceEmbeddingStatus,
        vector_count: usize,
        error_message: Option<&str>,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        set_source_embedding_status_tx(
            &tx,
            profile_id,
            source_id,
            status,
            vector_count,
            error_message,
        )?;
        tx.commit()?;
        Ok(())
    }

    pub(crate) fn set_source_embedding_failures(
        &self,
        profile_id: &EmbeddingProfileId,
        source_vector_counts: &[(SourceId, usize)],
        error_message: &str,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction()?;
        set_source_embedding_failures_tx(&tx, profile_id, source_vector_counts, error_message)?;
        tx.commit()?;
        Ok(())
    }

    // --- Task ---

    pub fn create_task(
        &self,
        task_id: &TaskId,
        kind: TaskKind,
        request: &serde_json::Value,
    ) -> Result<TaskSummary> {
        let now = unix_timestamp_string();
        let request = bounded_json(request.clone());
        let request_json = serde_json::to_string(&request).context("serialize task request")?;
        self.conn.execute(
            "INSERT INTO tasks (id, kind, status, created_at, updated_at, started_at, finished_at, request_json, result_json, error, progress_json)
             VALUES (?1, ?2, ?3, ?4, ?4, NULL, NULL, ?5, NULL, NULL, NULL)",
            params![
                &task_id.0,
                kind.as_str(),
                TaskStatus::Queued.as_str(),
                &now,
                request_json,
            ],
        )?;
        Ok(TaskSummary {
            id: task_id.clone(),
            kind,
            status: TaskStatus::Queued,
            created_at: now.clone(),
            updated_at: now,
            started_at: None,
            finished_at: None,
            request,
            result: None,
            error: None,
            queue_position: None,
            blocking_reason: None,
            progress: None,
        })
    }

    pub fn get_task(&self, task_id: &TaskId) -> Result<Option<TaskSummary>> {
        self.conn
            .prepare(
                "SELECT id, kind, status, created_at, updated_at, started_at, finished_at, request_json, result_json, error, progress_json
                 FROM tasks WHERE id = ?1",
            )?
            .query_row(params![&task_id.0], row_to_task_summary)
            .optional()
            .map_err(Into::into)
    }

    pub fn get_task_profile(&self, task_id: &TaskId) -> Result<Option<TaskProfile>> {
        let profile_json = self
            .conn
            .prepare("SELECT profile_json FROM tasks WHERE id = ?1")?
            .query_row(params![&task_id.0], |row| row.get::<_, Option<String>>(0))
            .optional()?
            .flatten();
        profile_json
            .map(|json| serde_json::from_str(&json).context("deserialize task profile"))
            .transpose()
    }

    pub fn task_status(&self, task_id: &TaskId) -> Result<Option<TaskStatus>> {
        self.conn
            .prepare("SELECT status FROM tasks WHERE id = ?1")?
            .query_row(params![&task_id.0], |row| {
                let status: String = row.get(0)?;
                TaskStatus::from_store_str(&status).ok_or_else(|| {
                    rusqlite::Error::FromSqlConversionFailure(
                        0,
                        Type::Text,
                        format!("invalid task status: {status}").into(),
                    )
                })
            })
            .optional()
            .map_err(Into::into)
    }

    pub fn start_task(&self, task_id: &TaskId) -> Result<bool> {
        let now = unix_timestamp_string();
        let changed = self.conn.execute(
            "UPDATE tasks
             SET status = ?2, started_at = COALESCE(started_at, ?3), updated_at = ?3
             WHERE id = ?1 AND status = ?4",
            params![
                &task_id.0,
                TaskStatus::Running.as_str(),
                now,
                TaskStatus::Queued.as_str(),
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn start_task_if_no_running(&self, task_id: &TaskId, kind: TaskKind) -> Result<bool> {
        let now = unix_timestamp_string();
        let changed = self.conn.execute(
            "UPDATE tasks
             SET status = ?2, started_at = COALESCE(started_at, ?3), updated_at = ?3
             WHERE id = ?1
               AND kind = ?4
               AND status = ?5
               AND NOT EXISTS (
                   SELECT 1 FROM tasks
                   WHERE kind = ?4 AND status = ?2
               )",
            params![
                &task_id.0,
                TaskStatus::Running.as_str(),
                now,
                kind.as_str(),
                TaskStatus::Queued.as_str(),
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn start_tasks_if_no_running(&self, task_ids: &[TaskId], kind: TaskKind) -> Result<bool> {
        if task_ids.is_empty() {
            return Ok(false);
        }
        let now = unix_timestamp_string();
        let tx = self.conn.unchecked_transaction()?;
        let running: i64 = tx.query_row(
            "SELECT COUNT(*) FROM tasks WHERE kind = ?1 AND status = ?2",
            params![kind.as_str(), TaskStatus::Running.as_str()],
            |row| row.get(0),
        )?;
        if running > 0 {
            return Ok(false);
        }
        for task_id in task_ids {
            let changed = tx.execute(
                "UPDATE tasks
                 SET status = ?2, started_at = COALESCE(started_at, ?3), updated_at = ?3
                 WHERE id = ?1
                   AND kind = ?4
                   AND status = ?5",
                params![
                    &task_id.0,
                    TaskStatus::Running.as_str(),
                    &now,
                    kind.as_str(),
                    TaskStatus::Queued.as_str(),
                ],
            )?;
            if changed != 1 {
                return Ok(false);
            }
        }
        tx.commit()?;
        Ok(true)
    }

    pub fn update_task_progress(
        &self,
        task_id: &TaskId,
        progress: TaskProgressSnapshot,
    ) -> Result<Option<TaskProgressSnapshot>> {
        let now = unix_timestamp_string();
        let progress = progress.bounded().with_current_elapsed();
        let progress_json =
            serde_json::to_string(&progress).context("serialize task progress snapshot")?;
        let progress_phase = progress.phase.as_ref().map(|phase| phase.name.as_str());
        let progress_phase_started_at = progress
            .phase
            .as_ref()
            .map(|phase| phase.started_at.as_str());
        let progress_wait_reason = progress.wait_reason.as_deref();
        let progress_recent_status = progress.recent_status.as_deref();
        let changed = self.conn.execute(
            "UPDATE tasks
             SET progress_json = ?2,
                 updated_at = ?3,
                 progress_phase = ?4,
                 progress_wait_reason = ?5,
                 progress_recent_status = ?6,
                 progress_phase_started_at = ?7
             WHERE id = ?1 AND status IN (?8, ?9)",
            params![
                &task_id.0,
                progress_json,
                now,
                progress_phase,
                progress_wait_reason,
                progress_recent_status,
                progress_phase_started_at,
                TaskStatus::Queued.as_str(),
                TaskStatus::Running.as_str(),
            ],
        )?;
        if changed == 0 {
            return Ok(None);
        }
        let payload = serde_json::to_value(&progress).context("serialize progress event")?;
        self.insert_task_event(task_id, "progress", "task progress", &payload)?;
        Ok(Some(progress))
    }

    pub fn count_running_tasks(&self, kind: TaskKind) -> Result<usize> {
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM tasks WHERE kind = ?1 AND status = ?2",
            params![kind.as_str(), TaskStatus::Running.as_str()],
            |row| row.get(0),
        )?;
        Ok(count.try_into().unwrap_or(usize::MAX))
    }

    pub fn next_queued_task(&self, kind: TaskKind) -> Result<Option<TaskSummary>> {
        self.conn
            .prepare(
                "SELECT id, kind, status, created_at, updated_at, started_at, finished_at, request_json, result_json, error, progress_json
                 FROM tasks
                 WHERE kind = ?1 AND status = ?2
                 ORDER BY created_at, id
                 LIMIT 1",
            )?
            .query_row(
                params![kind.as_str(), TaskStatus::Queued.as_str()],
                row_to_task_summary,
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn queued_tasks(&self, kind: TaskKind) -> Result<Vec<TaskSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, status, created_at, updated_at, started_at, finished_at, request_json, result_json, error, progress_json
             FROM tasks
             WHERE kind = ?1 AND status = ?2
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(
            params![kind.as_str(), TaskStatus::Queued.as_str()],
            row_to_task_summary,
        )?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn tasks(&self, kind: TaskKind) -> Result<Vec<TaskSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, status, created_at, updated_at, started_at, finished_at, request_json, result_json, error, progress_json
             FROM tasks
             WHERE kind = ?1
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(params![kind.as_str()], row_to_task_summary)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    /// Returns all tasks regardless of kind, ordered by creation time.
    pub fn tasks_all(&self) -> Result<Vec<TaskSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, status, created_at, updated_at, started_at, finished_at, request_json, result_json, error, progress_json
             FROM tasks
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map([], row_to_task_summary)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn active_tasks(&self, kind: TaskKind) -> Result<Vec<TaskSummary>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, kind, status, created_at, updated_at, started_at, finished_at, request_json, result_json, error, progress_json
             FROM tasks
             WHERE kind = ?1 AND status IN (?2, ?3)
             ORDER BY created_at, id",
        )?;
        let rows = stmt.query_map(
            params![
                kind.as_str(),
                TaskStatus::Queued.as_str(),
                TaskStatus::Running.as_str(),
            ],
            row_to_task_summary,
        )?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn list_tasks_page(&self, filter: TaskListFilter, limit: usize) -> Result<TaskListPage> {
        let limit = sql_limit(limit.max(1));
        let (tasks, total) = match filter {
            TaskListFilter::Active => {
                let total = self.conn.query_row(
                    "SELECT COUNT(*) FROM tasks WHERE status IN (?1, ?2)",
                    params![TaskStatus::Queued.as_str(), TaskStatus::Running.as_str()],
                    |row| row_usize(row, 0),
                )?;
                let mut stmt = self.conn.prepare(
                    "SELECT id, kind, status, created_at, updated_at, started_at, finished_at, request_json, result_json, error, progress_json
                     FROM tasks
                     WHERE status IN (?1, ?2)
                     ORDER BY created_at, id
                     LIMIT ?3",
                )?;
                let rows = stmt.query_map(
                    params![
                        TaskStatus::Queued.as_str(),
                        TaskStatus::Running.as_str(),
                        limit,
                    ],
                    row_to_task_summary,
                )?;
                let tasks = rows
                    .map(|row| row.map_err(Into::into))
                    .collect::<Result<_>>()?;
                (tasks, total)
            }
            TaskListFilter::All => {
                let total = self
                    .conn
                    .query_row("SELECT COUNT(*) FROM tasks", [], |row| row_usize(row, 0))?;
                let mut stmt = self.conn.prepare(
                    "SELECT id, kind, status, created_at, updated_at, started_at, finished_at, request_json, result_json, error, progress_json
                     FROM tasks
                     ORDER BY created_at, id
                     LIMIT ?1",
                )?;
                let rows = stmt.query_map(params![limit], row_to_task_summary)?;
                let tasks = rows
                    .map(|row| row.map_err(Into::into))
                    .collect::<Result<_>>()?;
                (tasks, total)
            }
        };
        Ok(TaskListPage { tasks, total })
    }

    pub fn task_turnover_window(&self, limit: usize) -> Result<TaskTurnoverWindow> {
        let event_limit = limit.max(1);
        let event_sequence_ceiling =
            self.conn
                .query_row("SELECT COALESCE(MAX(id), 0) FROM task_events", [], |row| {
                    row.get::<_, i64>(0)
                })?;
        let event_sequence_floor = event_sequence_ceiling
            .saturating_sub(sql_limit(event_limit).saturating_sub(1))
            .max(0);
        let mut window = TaskTurnoverWindow {
            event_sequence_floor,
            event_sequence_ceiling,
            event_limit,
            recent_succeeded: 0,
            recent_failed: 0,
            recent_cancelled: 0,
            recent_backfilled: 0,
        };
        if event_sequence_ceiling == 0 {
            return Ok(window);
        }

        let mut stmt = self.conn.prepare(
            "SELECT e.event_type, t.status, COUNT(*)
             FROM task_events e
             JOIN tasks t ON t.id = e.task_id
             WHERE e.id >= ?1 AND e.id <= ?2
             GROUP BY e.event_type, t.status",
        )?;
        let rows = stmt.query_map(
            params![event_sequence_floor, event_sequence_ceiling],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row_usize(row, 2)?,
                ))
            },
        )?;
        for row in rows {
            let (event_type, status, count) = row?;
            match event_type.as_str() {
                "succeeded" => {
                    window.recent_succeeded = window.recent_succeeded.saturating_add(count)
                }
                "failed" => window.recent_failed = window.recent_failed.saturating_add(count),
                "cancelled" => {
                    window.recent_cancelled = window.recent_cancelled.saturating_add(count);
                }
                "queued"
                    if matches!(
                        TaskStatus::from_store_str(&status),
                        Some(TaskStatus::Queued | TaskStatus::Running)
                    ) =>
                {
                    window.recent_backfilled = window.recent_backfilled.saturating_add(count);
                }
                _ => {}
            }
        }
        Ok(window)
    }

    pub fn active_task_metadata_aggregate(
        &self,
        reason_bucket_limit: usize,
    ) -> Result<TaskActiveMetadataAggregate> {
        let reason_bucket_limit = sql_limit(reason_bucket_limit.max(1));
        let (embedding_waiting, oldest_embedding_started_at) = self.conn.query_row(
            "SELECT COUNT(*), MIN(progress_phase_started_at)
             FROM tasks
             WHERE status IN (?1, ?2)
               AND progress_phase = ?3
               AND (progress_wait_reason IS NOT NULL OR progress_recent_status = 'waiting for embedding batch')",
            params![
                TaskStatus::Queued.as_str(),
                TaskStatus::Running.as_str(),
                IngestTaskStage::EmbeddingQueueWait.as_str()
            ],
            |row| Ok((row_usize(row, 0)?, row.get::<_, Option<String>>(1)?)),
        )?;
        let publish_complete_running = self.conn.query_row(
            "SELECT COUNT(*)
             FROM tasks
             WHERE status = ?1
               AND progress_recent_status = 'index publishing complete'",
            params![TaskStatus::Running.as_str()],
            |row| row_usize(row, 0),
        )?;
        Ok(TaskActiveMetadataAggregate {
            embedding_waiting,
            oldest_embedding_wait_ms: oldest_embedding_started_at
                .as_deref()
                .and_then(elapsed_ms_since_unix_seconds),
            embedding_reason_buckets: self
                .active_embedding_wait_reason_buckets(reason_bucket_limit)?,
            publish_complete_running,
            stale_reason_buckets: self
                .publish_complete_running_reason_buckets(reason_bucket_limit)?,
        })
    }

    fn active_embedding_wait_reason_buckets(&self, limit: i64) -> Result<Vec<TaskReasonCount>> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(progress_wait_reason, 'embedding_batch') AS reason, COUNT(*)
             FROM tasks
             WHERE status IN (?1, ?2)
               AND progress_phase = ?3
               AND (progress_wait_reason IS NOT NULL OR progress_recent_status = 'waiting for embedding batch')
             GROUP BY reason
             ORDER BY COUNT(*) DESC, reason
             LIMIT ?4",
        )?;
        let rows = stmt.query_map(
            params![
                TaskStatus::Queued.as_str(),
                TaskStatus::Running.as_str(),
                IngestTaskStage::EmbeddingQueueWait.as_str(),
                limit
            ],
            row_to_reason_count,
        )?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    fn publish_complete_running_reason_buckets(&self, limit: i64) -> Result<Vec<TaskReasonCount>> {
        let mut stmt = self.conn.prepare(
            "SELECT COALESCE(progress_wait_reason, 'post_publish_follow_up') AS reason, COUNT(*)
             FROM tasks
             WHERE status = ?1
               AND progress_recent_status = 'index publishing complete'
             GROUP BY reason
             ORDER BY COUNT(*) DESC, reason
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(
            params![TaskStatus::Running.as_str(), limit],
            row_to_reason_count,
        )?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn queued_task_position(&self, task_id: &TaskId) -> Result<Option<usize>> {
        let task = self
            .conn
            .prepare("SELECT kind, status, created_at FROM tasks WHERE id = ?1")?
            .query_row(params![&task_id.0], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })
            .optional()?;
        let Some((kind, status, created_at)) = task else {
            return Ok(None);
        };
        if status != TaskStatus::Queued.as_str() {
            return Ok(None);
        }
        let count: i64 = self.conn.query_row(
            "SELECT COUNT(*)
             FROM tasks
             WHERE kind = ?1
               AND status = ?2
               AND (created_at < ?3 OR (created_at = ?3 AND id <= ?4))",
            params![kind, TaskStatus::Queued.as_str(), created_at, &task_id.0,],
            |row| row.get(0),
        )?;
        Ok(Some(count.try_into().unwrap_or(usize::MAX)))
    }

    pub fn finish_task_success(
        &self,
        task_id: &TaskId,
        result: &serde_json::Value,
    ) -> Result<bool> {
        self.finish_task_success_with_optional_profile(task_id, result, None)
    }

    pub fn finish_task_success_with_profile(
        &self,
        task_id: &TaskId,
        result: &serde_json::Value,
        profile: &TaskProfile,
    ) -> Result<bool> {
        self.finish_task_success_with_optional_profile(task_id, result, Some(profile))
    }

    fn finish_task_success_with_optional_profile(
        &self,
        task_id: &TaskId,
        result: &serde_json::Value,
        profile: Option<&TaskProfile>,
    ) -> Result<bool> {
        let now = unix_timestamp_string();
        let result = serde_json::to_string(&bounded_json(result.clone()))
            .context("serialize task success result")?;
        let profile = profile
            .map(serde_json::to_string)
            .transpose()
            .context("serialize task profile")?;
        // Keep terminal status, result metadata, and profile JSON in one SQLite statement.
        let changed = self.conn.execute(
             "UPDATE tasks
             SET status = ?2, updated_at = ?3, finished_at = COALESCE(finished_at, ?3), result_json = ?4, profile_json = ?5, error = NULL
             WHERE id = ?1 AND status IN (?6, ?7)",
            params![
                &task_id.0,
                TaskStatus::Succeeded.as_str(),
                now,
                result,
                profile,
                TaskStatus::Queued.as_str(),
                TaskStatus::Running.as_str(),
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn finish_task_failed(&self, task_id: &TaskId, error: &str) -> Result<bool> {
        self.finish_task_failed_with_result(task_id, error, None)
    }

    pub fn finish_task_failed_with_result(
        &self,
        task_id: &TaskId,
        error: &str,
        result: Option<&serde_json::Value>,
    ) -> Result<bool> {
        let now = unix_timestamp_string();
        let result = result
            .cloned()
            .map(bounded_json)
            .map(|result| serde_json::to_string(&result))
            .transpose()
            .context("serialize task failure result metadata")?;
        let changed = self.conn.execute(
            "UPDATE tasks
             SET status = ?2, updated_at = ?3, finished_at = COALESCE(finished_at, ?3), error = ?4, result_json = ?5
             WHERE id = ?1 AND status IN (?6, ?7)",
            params![
                &task_id.0,
                TaskStatus::Failed.as_str(),
                now,
                bounded_error(error),
                result,
                TaskStatus::Queued.as_str(),
                TaskStatus::Running.as_str(),
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn resume_failed_task(&self, task_id: &TaskId) -> Result<bool> {
        let now = unix_timestamp_string();
        let changed = self.conn.execute(
            "UPDATE tasks
             SET status = ?2,
                 updated_at = ?3,
                 started_at = NULL,
                 finished_at = NULL,
                 result_json = NULL,
                 error = NULL,
                 progress_json = NULL,
                 progress_phase = NULL,
                 progress_wait_reason = NULL,
                 progress_recent_status = NULL,
                 progress_phase_started_at = NULL,
                 profile_json = NULL
             WHERE id = ?1 AND status = ?4",
            params![
                &task_id.0,
                TaskStatus::Queued.as_str(),
                now,
                TaskStatus::Failed.as_str(),
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn cancel_task(&self, task_id: &TaskId) -> Result<bool> {
        let now = unix_timestamp_string();
        let changed = self.conn.execute(
            "UPDATE tasks
             SET status = ?2, updated_at = ?3, finished_at = COALESCE(finished_at, ?3)
             WHERE id = ?1 AND status IN (?4, ?5)",
            params![
                &task_id.0,
                TaskStatus::Cancelled.as_str(),
                now,
                TaskStatus::Queued.as_str(),
                TaskStatus::Running.as_str(),
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn insert_task_event(
        &self,
        task_id: &TaskId,
        event_type: &str,
        message: &str,
        payload: &serde_json::Value,
    ) -> Result<TaskEvent> {
        let now = unix_timestamp_string();
        let payload = bounded_json(payload.clone());
        let payload_json =
            serde_json::to_string(&payload).context("serialize task event payload")?;
        let message = bounded_message(message);
        self.conn.execute(
            "INSERT INTO task_events (task_id, event_type, message, payload_json, created_at)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![&task_id.0, event_type, &message, payload_json, &now],
        )?;
        let sequence = self.conn.last_insert_rowid();
        Ok(TaskEvent {
            sequence,
            task_id: task_id.clone(),
            event_type: event_type.to_string(),
            message,
            payload,
            created_at: now,
        })
    }

    pub fn list_task_events(
        &self,
        task_id: &TaskId,
        after_sequence: Option<i64>,
        limit: usize,
    ) -> Result<Vec<TaskEvent>> {
        let after_sequence = after_sequence.unwrap_or_default();
        let limit = sql_limit(limit);
        let mut stmt = self.conn.prepare(
            "SELECT id, task_id, event_type, message, payload_json, created_at
             FROM task_events
             WHERE task_id = ?1 AND id > ?2
             ORDER BY id
             LIMIT ?3",
        )?;
        let rows = stmt.query_map(
            params![&task_id.0, after_sequence, limit],
            row_to_task_event,
        )?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }

    pub fn insert_task_span(
        &self,
        task_id: &TaskId,
        phase: &str,
        started_at: &str,
        duration_ms: u64,
        metadata: &serde_json::Value,
    ) -> Result<TaskSpan> {
        let existing: i64 = self.conn.query_row(
            "SELECT COUNT(*) FROM task_spans WHERE task_id = ?1",
            params![&task_id.0],
            |row| row.get(0),
        )?;
        if existing >= TASK_SPAN_MAX_PER_TASK as i64 {
            return Ok(TaskSpan {
                sequence: 0,
                task_id: task_id.clone(),
                phase: phase.to_string(),
                started_at: started_at.to_string(),
                duration_ms,
                metadata: serde_json::json!({
                    "dropped": true,
                    "reason": "task_span_limit_reached",
                    "max_spans": TASK_SPAN_MAX_PER_TASK,
                }),
            });
        }
        let metadata = bounded_json(metadata.clone());
        let metadata_json =
            serde_json::to_string(&metadata).context("serialize task span metadata")?;
        self.conn.execute(
            "INSERT INTO task_spans (task_id, phase, started_at, duration_ms, metadata_json)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![
                &task_id.0,
                phase,
                started_at,
                duration_ms.min(i64::MAX as u64) as i64,
                metadata_json,
            ],
        )?;
        let sequence = self.conn.last_insert_rowid();
        Ok(TaskSpan {
            sequence,
            task_id: task_id.clone(),
            phase: phase.to_string(),
            started_at: started_at.to_string(),
            duration_ms,
            metadata,
        })
    }

    pub fn list_task_spans(&self, task_id: &TaskId) -> Result<Vec<TaskSpan>> {
        let mut stmt = self.conn.prepare(
            "SELECT id, task_id, phase, started_at, duration_ms, metadata_json
             FROM task_spans
             WHERE task_id = ?1
             ORDER BY id",
        )?;
        let rows = stmt.query_map(params![&task_id.0], row_to_task_span)?;
        rows.map(|row| row.map_err(Into::into)).collect()
    }
}

fn row_to_task_summary(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskSummary> {
    let kind_text: String = row.get(1)?;
    let status_text: String = row.get(2)?;
    let request_json: String = row.get(7)?;
    let result_json: Option<String> = row.get(8)?;
    let progress_json: Option<String> = row.get(10)?;
    let request = serde_json::from_str(&request_json)
        .map_err(|err| from_json_error(7, "serde_json::Value", err))?;
    let result = result_json
        .map(|json| {
            serde_json::from_str(&json).map_err(|err| from_json_error(8, "serde_json::Value", err))
        })
        .transpose()?;
    let progress = progress_json
        .map(|json| {
            serde_json::from_str::<TaskProgressSnapshot>(&json)
                .map(TaskProgressSnapshot::with_current_elapsed)
                .map_err(|err| from_json_error(10, "TaskProgressSnapshot", err))
        })
        .transpose()?;
    let kind = TaskKind::from_store_str(&kind_text)
        .ok_or_else(|| invalid_text_value(1, format!("unknown task kind: {kind_text}")))?;
    let status = TaskStatus::from_store_str(&status_text)
        .ok_or_else(|| invalid_text_value(2, format!("unknown task status: {status_text}")))?;

    Ok(TaskSummary {
        id: TaskId(row.get(0)?),
        kind,
        status,
        created_at: row.get(3)?,
        updated_at: row.get(4)?,
        started_at: row.get(5)?,
        finished_at: row.get(6)?,
        request,
        result,
        error: row.get(9)?,
        queue_position: None,
        blocking_reason: None,
        progress,
    })
}

fn row_to_reason_count(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskReasonCount> {
    Ok(TaskReasonCount {
        reason: row.get(0)?,
        count: row_usize(row, 1)?,
    })
}

fn row_to_task_event(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskEvent> {
    let payload_json: String = row.get(4)?;
    let payload = serde_json::from_str(&payload_json)
        .map_err(|err| from_json_error(4, "serde_json::Value", err))?;
    Ok(TaskEvent {
        sequence: row.get(0)?,
        task_id: TaskId(row.get(1)?),
        event_type: row.get(2)?,
        message: row.get(3)?,
        payload,
        created_at: row.get(5)?,
    })
}

fn row_to_task_span(row: &rusqlite::Row<'_>) -> rusqlite::Result<TaskSpan> {
    let metadata_json: String = row.get(5)?;
    let metadata = serde_json::from_str(&metadata_json)
        .map_err(|err| from_json_error(5, "serde_json::Value", err))?;
    let duration_ms: i64 = row.get(4)?;
    Ok(TaskSpan {
        sequence: row.get(0)?,
        task_id: TaskId(row.get(1)?),
        phase: row.get(2)?,
        started_at: row.get(3)?,
        duration_ms: duration_ms.try_into().unwrap_or_default(),
        metadata,
    })
}

fn ensure_column(conn: &Connection, table: &str, column: &str, alter_sql: &str) -> Result<()> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(());
        }
    }
    conn.execute_batch(alter_sql)?;
    Ok(())
}

fn backfill_task_progress_metadata(conn: &Connection) -> Result<()> {
    conn.execute(
        "UPDATE tasks
         SET progress_phase = json_extract(progress_json, '$.phase.name'),
             progress_wait_reason = json_extract(progress_json, '$.wait_reason'),
             progress_recent_status = json_extract(progress_json, '$.recent_status'),
             progress_phase_started_at = json_extract(progress_json, '$.phase.started_at')
         WHERE progress_json IS NOT NULL
           AND progress_phase IS NULL
           AND progress_wait_reason IS NULL
           AND progress_recent_status IS NULL
           AND progress_phase_started_at IS NULL",
        [],
    )?;
    Ok(())
}

fn migrate_embedding_profile_tables(conn: &Connection) -> Result<()> {
    let now = unix_timestamp_string();
    let had_query_instruction_hash =
        table_has_column(conn, "embedding_profiles", "query_instruction_hash")?;
    let had_document_instruction_hash =
        table_has_column(conn, "embedding_profiles", "document_instruction_hash")?;
    ensure_column(
        conn,
        "embedding_profiles",
        "query_instruction_hash",
        "ALTER TABLE embedding_profiles ADD COLUMN query_instruction_hash TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        conn,
        "embedding_profiles",
        "document_instruction_hash",
        "ALTER TABLE embedding_profiles ADD COLUMN document_instruction_hash TEXT NOT NULL DEFAULT ''",
    )?;
    ensure_column(
        conn,
        "embedding_profiles",
        "endpoint_identity",
        "ALTER TABLE embedding_profiles ADD COLUMN endpoint_identity TEXT",
    )?;
    ensure_column(
        conn,
        "embedding_profiles",
        "requested_model",
        "ALTER TABLE embedding_profiles ADD COLUMN requested_model TEXT",
    )?;
    ensure_column(
        conn,
        "embedding_profiles",
        "served_model",
        "ALTER TABLE embedding_profiles ADD COLUMN served_model TEXT",
    )?;
    ensure_column(
        conn,
        "embedding_profiles",
        "max_context_tokens",
        "ALTER TABLE embedding_profiles ADD COLUMN max_context_tokens INTEGER",
    )?;
    ensure_column(
        conn,
        "embedding_profiles",
        "dtype",
        "ALTER TABLE embedding_profiles ADD COLUMN dtype TEXT",
    )?;
    ensure_column(
        conn,
        "embedding_profiles",
        "quantization",
        "ALTER TABLE embedding_profiles ADD COLUMN quantization TEXT",
    )?;
    ensure_column(
        conn,
        "embedding_profiles",
        "weight_identity",
        "ALTER TABLE embedding_profiles ADD COLUMN weight_identity TEXT",
    )?;
    ensure_column(
        conn,
        "embedding_profiles",
        "chunker_version",
        "ALTER TABLE embedding_profiles ADD COLUMN chunker_version TEXT NOT NULL DEFAULT 'parent-child-v2'",
    )?;
    ensure_column(
        conn,
        "embedding_profiles",
        "child_target_tokens",
        "ALTER TABLE embedding_profiles ADD COLUMN child_target_tokens INTEGER NOT NULL DEFAULT 300",
    )?;
    ensure_column(
        conn,
        "embedding_profiles",
        "child_overlap_tokens",
        "ALTER TABLE embedding_profiles ADD COLUMN child_overlap_tokens INTEGER NOT NULL DEFAULT 80",
    )?;
    ensure_column(
        conn,
        "embedding_profiles",
        "parent_children_count",
        "ALTER TABLE embedding_profiles ADD COLUMN parent_children_count INTEGER NOT NULL DEFAULT 5",
    )?;
    ensure_column(
        conn,
        "embedding_profiles",
        "embedding_input_budget_tokens",
        "ALTER TABLE embedding_profiles ADD COLUMN embedding_input_budget_tokens INTEGER",
    )?;
    if !had_query_instruction_hash || !had_document_instruction_hash {
        backfill_embedding_profile_instruction_hashes(conn)?;
    }

    conn.execute(
        "INSERT OR IGNORE INTO embedding_profiles
            (id, provider, model, dimension, normalize, query_instruction_hash, document_instruction_hash, config_hash, qdrant_collection, qdrant_vector_name, created_at, updated_at)
         VALUES (?1, 'legacy', 'legacy', 0, 1, '', '', ?2, NULL, NULL, ?3, ?3)",
        params![
            DEFAULT_EMBEDDING_PROFILE_ID,
            LEGACY_EMBEDDING_PROFILE_CONFIG_HASH,
            now
        ],
    )?;

    if !table_has_column(conn, "chunk_vectors", "profile_id")? {
        conn.execute_batch(
            "
            ALTER TABLE chunk_vectors RENAME TO chunk_vectors_legacy_profile_migration;
            CREATE TABLE chunk_vectors (
                profile_id TEXT NOT NULL REFERENCES embedding_profiles(id) ON DELETE CASCADE,
                chunk_id TEXT NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
                source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
                vector_json TEXT NOT NULL,
                PRIMARY KEY (profile_id, chunk_id)
            );
            INSERT INTO chunk_vectors (profile_id, chunk_id, source_id, vector_json)
                SELECT 'default', chunk_id, source_id, vector_json
                FROM chunk_vectors_legacy_profile_migration;
            DROP TABLE chunk_vectors_legacy_profile_migration;
            ",
        )?;
    }

    if !table_has_column(conn, "embeddings_meta", "profile_id")? {
        conn.execute_batch(
            "
            ALTER TABLE embeddings_meta RENAME TO embeddings_meta_legacy_profile_migration;
            CREATE TABLE embeddings_meta (
                profile_id TEXT NOT NULL REFERENCES embedding_profiles(id) ON DELETE CASCADE,
                chunk_id TEXT NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
                hnsw_position INTEGER NOT NULL,
                embedding_model TEXT NOT NULL,
                embedded_at TEXT NOT NULL,
                PRIMARY KEY (profile_id, chunk_id)
            );
            INSERT INTO embeddings_meta (profile_id, chunk_id, hnsw_position, embedding_model, embedded_at)
                SELECT 'default', chunk_id, hnsw_position, embedding_model, embedded_at
                FROM embeddings_meta_legacy_profile_migration;
            DROP TABLE embeddings_meta_legacy_profile_migration;
            ",
        )?;
    }

    let legacy_generation: Option<String> = conn
        .query_row(
            "SELECT value FROM index_meta WHERE key = 'generation'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    conn.execute(
        "INSERT OR IGNORE INTO embedding_profile_index_meta (profile_id, generation)
         VALUES (?1, ?2)",
        params![
            DEFAULT_EMBEDDING_PROFILE_ID,
            legacy_generation.as_deref().unwrap_or("0"),
        ],
    )?;
    conn.execute_batch(
        "CREATE INDEX IF NOT EXISTS chunk_vectors_profile_source_idx
            ON chunk_vectors(profile_id, source_id);",
    )?;

    // ── Vector BLOB migration (#171) ───────────────────────────────
    // Add vector_blob BLOB columns alongside the legacy vector_json TEXT
    // columns, then backfill from JSON in Rust (SQL can't parse JSON arrays
    // to little-endian f32 bytes).  New writes store vector_blob and leave
    // vector_json empty; reads prefer vector_blob and fall back to vector_json
    // for compatibility.
    if !table_has_column(conn, "chunk_vectors", "vector_blob")? {
        conn.execute_batch("ALTER TABLE chunk_vectors ADD COLUMN vector_blob BLOB;")?;
    }
    if !table_has_column(conn, "embedding_cache", "vector_blob")? {
        conn.execute_batch("ALTER TABLE embedding_cache ADD COLUMN vector_blob BLOB;")?;
    }
    // Backfill chunk_vectors.vector_blob from vector_json.
    backfill_vector_blobs(conn, "chunk_vectors")?;
    // Backfill embedding_cache.vector_blob from vector_json.
    backfill_vector_blobs(conn, "embedding_cache")?;
    Ok(())
}

/// Populate the vector_blob column from vector_json for rows where
/// vector_blob IS NULL and vector_json is non-empty.
fn backfill_vector_blobs(conn: &Connection, table: &str) -> Result<()> {
    // Query rows needing backfill in batches to avoid loading the entire
    // table into memory at once.  Uses unchecked_transaction() which works
    // on a shared &Connection reference (we hold no other active tx).
    loop {
        let mut select = conn.prepare(&format!(
            "SELECT rowid, vector_json FROM {table}
             WHERE vector_blob IS NULL AND vector_json IS NOT NULL AND vector_json != ''
             LIMIT 500"
        ))?;
        let rows: Vec<(i64, String)> = select
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?
            .filter_map(|r| r.ok())
            .collect();
        drop(select);

        if rows.is_empty() {
            break;
        }

        let tx = conn.unchecked_transaction()?;
        {
            let mut stmt = tx.prepare(&format!(
                "UPDATE {table} SET vector_blob = ?1 WHERE rowid = ?2"
            ))?;
            for (rowid, json) in &rows {
                match serde_json::from_str::<Vec<f32>>(json) {
                    Ok(vector) => {
                        let blob = vector_to_blob(&vector);
                        stmt.execute(params![blob, rowid])?;
                    }
                    Err(_) => {
                        // Mark malformed JSON as processed with an empty BLOB
                        // so it doesn't cause an infinite loop on re-query.
                        stmt.execute(params![Vec::<u8>::new(), rowid])?;
                    }
                }
            }
        }
        tx.commit()?;
    }
    Ok(())
}

fn table_has_column(conn: &Connection, table: &str, column: &str) -> Result<bool> {
    let mut stmt = conn.prepare(&format!("PRAGMA table_info({table})"))?;
    let rows = stmt.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column {
            return Ok(true);
        }
    }
    Ok(false)
}

fn backfill_embedding_profile_instruction_hashes(conn: &Connection) -> Result<()> {
    let rows = {
        let mut stmt = conn.prepare(
            "SELECT id, provider, model, dimension, normalize, config_hash
             FROM embedding_profiles",
        )?;
        let rows = stmt.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, bool>(4)?,
                row.get::<_, String>(5)?,
            ))
        })?;
        rows.collect::<rusqlite::Result<Vec<_>>>()?
    };
    for (id, provider, model, dimension, normalize, old_config_hash) in rows {
        if old_config_hash == LEGACY_EMBEDDING_PROFILE_CONFIG_HASH {
            continue;
        }
        let dimension = usize::try_from(dimension).with_context(|| {
            format!("invalid embedding profile dimension for profile '{id}': {dimension}")
        })?;
        let config = EmbeddingProfileConfig {
            provider: &provider,
            model: &model,
            dimension,
            normalize,
            endpoint_identity: None,
            requested_model: None,
            served_model: None,
            max_context_tokens: None,
            dtype: None,
            quantization: None,
            weight_identity: None,
            chunker_version: "parent-child-v2",
            child_target_tokens: 300,
            child_overlap_tokens: 80,
            parent_children_count: 5,
            embedding_input_budget_tokens: None,
            query_instruction: "",
            document_instruction: "",
        };
        let empty_instruction_hash = config.query_instruction_hash();
        let config_hash = config.config_hash();
        conn.execute(
            "UPDATE embedding_profiles
             SET query_instruction_hash = ?2,
                 document_instruction_hash = ?2,
                 config_hash = ?3
             WHERE id = ?1",
            params![id, &empty_instruction_hash, config_hash],
        )?;
    }
    Ok(())
}

fn replace_source_contents_tx(
    tx: &Transaction<'_>,
    replacement: SourceContentsReplacement<'_>,
    deleted_child_chunks: usize,
) -> Result<SourceLexicalIndexUpdate> {
    let SourceContentsReplacement {
        source,
        evidence,
        chunks,
        embedding_profile_id,
        vectors,
        links,
        evidence_spans,
        image_artifacts,
        graph_nodes,
        graph_edges,
    } = replacement;
    let mut lexical_update = SourceLexicalIndexUpdate::start(deleted_child_chunks);
    let lexical_delete_started = Instant::now();
    tx.execute("DELETE FROM sources WHERE id = ?1", params![&source.id.0])?;
    lexical_update.add_elapsed_since(lexical_delete_started);
    tx.execute(
        "INSERT INTO sources (id, path, hash, status, parser_used, last_ingested_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            &source.id.0,
            source.path.to_str().unwrap_or(""),
            &source.hash,
            status_to_str(&source.status),
            &source.parser_used,
            &source.last_ingested_at,
        ],
    )?;

    insert_evidence_units_tx(tx, evidence)?;

    {
        let mut stmt = tx.prepare(
            "INSERT INTO chunks (id, source_id, chunk_hash, embedding_input_hash, text, context_text, token_count, chunk_type, parent_chunk_id, heading_path_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)"
        )?;
        for chunk in chunks {
            let heading_json =
                serde_json::to_string(&chunk.heading_path).context("serialize heading_path")?;
            if chunk.chunk_type == ChunkType::Child {
                let lexical_insert_started = Instant::now();
                stmt.execute(params![
                    &chunk.id.0,
                    &chunk.source_id.0,
                    &chunk.chunk_hash,
                    &chunk.embedding_input_hash,
                    &chunk.text,
                    &chunk.context_text,
                    chunk.token_count,
                    chunk_type_to_str(&chunk.chunk_type),
                    chunk.parent_chunk_id.as_ref().map(|id| &id.0),
                    heading_json,
                ])?;
                lexical_update.add_elapsed_since(lexical_insert_started);
                lexical_update.indexed_child_chunks =
                    lexical_update.indexed_child_chunks.saturating_add(1);
            } else {
                stmt.execute(params![
                    &chunk.id.0,
                    &chunk.source_id.0,
                    &chunk.chunk_hash,
                    &chunk.embedding_input_hash,
                    &chunk.text,
                    &chunk.context_text,
                    chunk.token_count,
                    chunk_type_to_str(&chunk.chunk_type),
                    chunk.parent_chunk_id.as_ref().map(|id| &id.0),
                    heading_json,
                ])?;
            }
        }
    }

    {
        let mut stmt = tx.prepare(
            "INSERT OR IGNORE INTO chunk_evidence (chunk_id, evidence_unit_id) VALUES (?1, ?2)",
        )?;
        for (chunk_id, evidence_id) in links {
            stmt.execute(params![&chunk_id.0, &evidence_id.0])?;
        }
    }

    evidence_spans::insert_chunk_evidence_spans_tx(tx, evidence_spans)?;
    insert_image_artifacts_tx(tx, image_artifacts)?;
    upsert_graph_nodes_tx(tx, graph_nodes)?;
    upsert_graph_edges_tx(tx, graph_edges)?;
    replace_source_vector_documents_for_profile_tx(tx, embedding_profile_id, &source.id, vectors)?;
    set_source_embedding_status_tx(
        tx,
        embedding_profile_id,
        &source.id,
        SourceEmbeddingStatus::Embedded,
        vectors.len(),
        None,
    )?;
    Ok(lexical_update)
}

fn insert_evidence_units_tx(tx: &Transaction<'_>, units: &[EvidenceUnit]) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO evidence_units (id, source_id, kind, locator_json, text, text_hash, heading_path_json, position, derived_from_evidence_id) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)"
    )?;
    for unit in units {
        let locator_json = serde_json::to_string(&unit.locator).context("serialize locator")?;
        let heading_json =
            serde_json::to_string(&unit.heading_path).context("serialize heading_path")?;
        stmt.execute(params![
            &unit.id.0,
            &unit.source_id.0,
            evidence_kind_to_str(unit.kind),
            locator_json,
            &unit.text,
            &unit.text_hash,
            heading_json,
            unit.position,
            unit.derived_from.as_ref().map(|id| &id.0),
        ])?;
    }
    Ok(())
}

fn row_to_evidence_unit(row: &rusqlite::Row<'_>) -> rusqlite::Result<EvidenceUnit> {
    let id: String = row.get(0)?;
    let source_id: String = row.get(1)?;
    let kind: String = row.get(2)?;
    let locator_json: String = row.get(3)?;
    let text: String = row.get(4)?;
    let text_hash: String = row.get(5)?;
    let heading_json: String = row.get(6)?;
    let position: u32 = row.get(7)?;
    let derived_from: Option<String> = row.get(8)?;

    let locator = serde_json::from_str(&locator_json)
        .map_err(|err| from_json_error(3, "SourceLocator", err))?;
    let heading_path = serde_json::from_str(&heading_json)
        .map_err(|err| from_json_error(6, "Vec<String>", err))?;

    Ok(EvidenceUnit {
        id: EvidenceId(id),
        source_id: SourceId(source_id),
        kind: str_to_evidence_kind(&kind),
        derived_from: derived_from.map(EvidenceId),
        locator,
        text,
        text_hash,
        heading_path,
        position,
    })
}

fn insert_image_artifacts_tx(tx: &Transaction<'_>, artifacts: &[ImageArtifact]) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO image_artifacts (image_id, source_id, evidence_unit_id, relative_path, content_hash, mime_type, width, height, page, image_index, bbox_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)"
    )?;
    for artifact in artifacts {
        let bbox_json = artifact
            .bbox
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serialize image bbox")?;
        stmt.execute(params![
            &artifact.image_id.0,
            &artifact.source_id.0,
            &artifact.evidence_id.0,
            artifact.relative_path.to_string_lossy(),
            &artifact.content_hash,
            &artifact.mime_type,
            artifact.width,
            artifact.height,
            artifact.page,
            artifact.image_index,
            bbox_json,
        ])?;
    }
    Ok(())
}

fn upsert_graph_nodes_tx(tx: &Transaction<'_>, nodes: &[GraphNode]) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO graph_nodes (id, source_id, kind, external_id, label, locator_json, ordinal, metadata_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
            source_id = excluded.source_id,
            kind = excluded.kind,
            external_id = excluded.external_id,
            label = excluded.label,
            locator_json = excluded.locator_json,
            ordinal = excluded.ordinal,
            metadata_json = excluded.metadata_json",
    )?;
    for node in nodes {
        let locator_json = node
            .locator
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serialize graph node locator")?;
        let metadata_json = node
            .metadata
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serialize graph node metadata")?;
        stmt.execute(params![
            &node.id.0,
            &node.source_id.0,
            node.kind.as_str(),
            &node.external_id,
            node.label.as_deref(),
            locator_json,
            node.ordinal,
            metadata_json,
        ])?;
    }
    Ok(())
}

fn upsert_graph_edges_tx(tx: &Transaction<'_>, edges: &[GraphEdge]) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO graph_edges (id, source_id, edge_type, from_node_id, to_node_id, ordinal, weight, metadata_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
         ON CONFLICT(id) DO UPDATE SET
            source_id = excluded.source_id,
            edge_type = excluded.edge_type,
            from_node_id = excluded.from_node_id,
            to_node_id = excluded.to_node_id,
            ordinal = excluded.ordinal,
            weight = excluded.weight,
            metadata_json = excluded.metadata_json",
    )?;
    for edge in edges {
        let metadata_json = edge
            .metadata
            .as_ref()
            .map(serde_json::to_string)
            .transpose()
            .context("serialize graph edge metadata")?;
        stmt.execute(params![
            &edge.id.0,
            &edge.source_id.0,
            edge.edge_type.as_str(),
            &edge.from_node_id.0,
            &edge.to_node_id.0,
            edge.ordinal,
            edge.weight,
            metadata_json,
        ])?;
    }
    Ok(())
}

fn row_to_image_artifact(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImageArtifact> {
    let bbox_json: Option<String> = row.get(10)?;
    let bbox = match bbox_json {
        Some(json) => {
            Some(serde_json::from_str(&json).map_err(|err| from_json_error(10, "BBox", err))?)
        }
        None => None,
    };

    Ok(ImageArtifact {
        image_id: ImageId(row.get(0)?),
        source_id: SourceId(row.get(1)?),
        evidence_id: EvidenceId(row.get(2)?),
        relative_path: PathBuf::from(row.get::<_, String>(3)?),
        content_hash: row.get(4)?,
        mime_type: row.get(5)?,
        width: row.get(6)?,
        height: row.get(7)?,
        page: row.get(8)?,
        image_index: row.get(9)?,
        bbox,
    })
}

fn row_to_graph_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphNode> {
    let kind_text: String = row.get(2)?;
    let locator_json: Option<String> = row.get(5)?;
    let metadata_json: Option<String> = row.get(7)?;
    let locator = locator_json
        .map(|json| {
            serde_json::from_str(&json).map_err(|err| from_json_error(5, "SourceLocator", err))
        })
        .transpose()?;
    let metadata = metadata_json
        .map(|json| {
            serde_json::from_str(&json).map_err(|err| from_json_error(7, "serde_json::Value", err))
        })
        .transpose()?;

    Ok(GraphNode {
        id: GraphNodeId(row.get(0)?),
        source_id: SourceId(row.get(1)?),
        kind: str_to_graph_node_kind(&kind_text, 2)?,
        external_id: row.get(3)?,
        label: row.get(4)?,
        locator,
        ordinal: row.get(6)?,
        metadata,
    })
}

fn row_to_graph_edge(row: &rusqlite::Row<'_>) -> rusqlite::Result<GraphEdge> {
    let edge_type_text: String = row.get(2)?;
    let metadata_json: Option<String> = row.get(7)?;
    let metadata = metadata_json
        .map(|json| {
            serde_json::from_str(&json).map_err(|err| from_json_error(7, "serde_json::Value", err))
        })
        .transpose()?;

    Ok(GraphEdge {
        id: GraphEdgeId(row.get(0)?),
        source_id: SourceId(row.get(1)?),
        edge_type: str_to_edge_type(&edge_type_text, 2)?,
        from_node_id: GraphNodeId(row.get(3)?),
        to_node_id: GraphNodeId(row.get(4)?),
        ordinal: row.get(5)?,
        weight: row.get(6)?,
        metadata,
    })
}

fn sql_limit(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

fn sql_usize(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn sql_opt_usize(value: Option<usize>) -> Option<i64> {
    value.map(sql_usize)
}

fn row_usize(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<usize> {
    let value = row.get::<_, i64>(idx)?;
    value
        .try_into()
        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(idx, value))
}

fn row_opt_usize(row: &rusqlite::Row<'_>, idx: usize) -> rusqlite::Result<Option<usize>> {
    row.get::<_, Option<i64>>(idx)?
        .map(|value| {
            value
                .try_into()
                .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(idx, value))
        })
        .transpose()
}

fn embedding_profile_config_hash(config: &EmbeddingProfileConfig<'_>) -> String {
    let query_instruction_hash = config.query_instruction_hash();
    let document_instruction_hash = config.document_instruction_hash();
    hex_sha256(
        format!(
            "{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}\0{}",
            config.provider,
            config.model,
            config.dimension,
            config.normalize,
            config.endpoint_identity.unwrap_or(""),
            config.requested_model.unwrap_or(""),
            config.served_model.unwrap_or(""),
            config
                .max_context_tokens
                .map(|value| value.to_string())
                .unwrap_or_default(),
            config.dtype.unwrap_or(""),
            config.quantization.unwrap_or(""),
            config.weight_identity.unwrap_or(""),
            config.chunker_version,
            config.child_target_tokens,
            config.child_overlap_tokens,
            config.parent_children_count,
            config
                .embedding_input_budget_tokens
                .map(|value| value.to_string())
                .unwrap_or_default(),
            query_instruction_hash,
            document_instruction_hash
        )
        .as_bytes(),
    )
}

fn update_embedding_profile_config_tx(
    tx: &Transaction<'_>,
    profile_id: &EmbeddingProfileId,
    config: EmbeddingProfileConfig<'_>,
    query_instruction_hash: &str,
    document_instruction_hash: &str,
    config_hash: &str,
) -> Result<()> {
    let now = unix_timestamp_string();
    tx.execute(
        "UPDATE embedding_profiles
         SET provider = ?2,
             model = ?3,
             dimension = ?4,
             normalize = ?5,
             endpoint_identity = ?6,
             requested_model = ?7,
             served_model = ?8,
             max_context_tokens = ?9,
             dtype = ?10,
             quantization = ?11,
             weight_identity = ?12,
             chunker_version = ?13,
             child_target_tokens = ?14,
             child_overlap_tokens = ?15,
             parent_children_count = ?16,
             embedding_input_budget_tokens = ?17,
             query_instruction_hash = ?18,
             document_instruction_hash = ?19,
             config_hash = ?20,
             updated_at = ?21
         WHERE id = ?1",
        params![
            profile_id.as_str(),
            config.provider,
            config.model,
            sql_usize(config.dimension),
            config.normalize,
            config.endpoint_identity,
            config.requested_model,
            config.served_model,
            sql_opt_usize(config.max_context_tokens),
            config.dtype,
            config.quantization,
            config.weight_identity,
            config.chunker_version,
            sql_usize(config.child_target_tokens),
            sql_usize(config.child_overlap_tokens),
            sql_usize(config.parent_children_count),
            sql_opt_usize(config.embedding_input_budget_tokens),
            query_instruction_hash,
            document_instruction_hash,
            config_hash,
            now,
        ],
    )?;
    Ok(())
}

fn clear_profile_vector_state_tx(
    tx: &Transaction<'_>,
    profile_id: &EmbeddingProfileId,
) -> Result<()> {
    tx.execute(
        "DELETE FROM chunk_vectors WHERE profile_id = ?1",
        params![profile_id.as_str()],
    )?;
    tx.execute(
        "DELETE FROM embedding_cache WHERE profile_id = ?1",
        params![profile_id.as_str()],
    )?;
    tx.execute(
        "DELETE FROM source_embedding_status WHERE profile_id = ?1",
        params![profile_id.as_str()],
    )?;
    tx.execute(
        "DELETE FROM embeddings_meta WHERE profile_id = ?1",
        params![profile_id.as_str()],
    )?;
    let _ = bump_profile_index_generation(tx, profile_id)?;
    Ok(())
}

fn embedding_instruction_hash(instruction: &str) -> String {
    hex_sha256(instruction.as_bytes())
}

fn row_to_image_caption_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<ImageCaptionRecord> {
    let caption_json: Option<String> = row.get(5)?;
    let caption = match caption_json {
        Some(json) => Some(
            serde_json::from_str::<ImageCaption>(&json)
                .map_err(|err| from_json_error(5, "ImageCaption", err))?,
        ),
        None => None,
    };
    let attempt_count: i64 = row.get(8)?;
    let cache_hits: i64 = row.get(9)?;

    Ok(ImageCaptionRecord {
        image_hash: row.get(0)?,
        model: row.get(1)?,
        prompt_version: row.get(2)?,
        prompt_hash: row.get(3)?,
        status: str_to_image_caption_status(&row.get::<_, String>(4)?),
        caption,
        raw_response: row.get(6)?,
        error_message: row.get(7)?,
        attempt_count: attempt_count.try_into().unwrap_or(0),
        cache_hits: cache_hits.try_into().unwrap_or(0),
        created_at: row.get(10)?,
        updated_at: row.get(11)?,
    })
}

fn from_json_error(idx: usize, ty: &'static str, err: serde_json::Error) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        idx,
        rusqlite::types::Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("parse {ty}: {err}"),
        )),
    )
}

type ChunkTuple = (
    String,
    String,
    String,
    Option<String>,
    String,
    Option<String>,
    u32,
    String,
    Option<String>,
    String,
);

fn row_to_chunk_tuple(row: &rusqlite::Row<'_>) -> rusqlite::Result<ChunkTuple> {
    Ok((
        row.get(0)?,
        row.get(1)?,
        row.get(2)?,
        row.get(3)?,
        row.get(4)?,
        row.get(5)?,
        row.get(6)?,
        row.get(7)?,
        row.get(8)?,
        row.get(9)?,
    ))
}

fn tuple_to_chunk(t: ChunkTuple, conn: &Connection) -> Result<Chunk> {
    let (
        id,
        source_id,
        chunk_hash,
        embedding_input_hash,
        text,
        context_text,
        token_count,
        chunk_type,
        parent_id,
        heading_json,
    ) = t;
    let evidence_unit_ids = get_evidence_ids_for_chunk(conn, &id)?;
    Ok(Chunk {
        id: ChunkId(id),
        source_id: SourceId(source_id),
        chunk_hash,
        embedding_input_hash,
        text,
        context_text,
        token_count,
        chunk_type: str_to_chunk_type(&chunk_type),
        parent_chunk_id: parent_id.map(ChunkId),
        heading_path: serde_json::from_str(&heading_json)?,
        evidence_unit_ids,
    })
}

fn get_evidence_ids_for_chunk(conn: &Connection, chunk_id: &str) -> Result<Vec<EvidenceId>> {
    let mut stmt =
        conn.prepare("SELECT evidence_unit_id FROM chunk_evidence WHERE chunk_id = ?1")?;
    let rows = stmt.query_map(params![chunk_id], |row| row.get::<_, String>(0))?;
    rows.map(|r| Ok(EvidenceId(r?))).collect()
}

fn sorted_source_filter_ids(source_ids: &HashSet<SourceId>) -> Vec<String> {
    let mut source_ids = source_ids
        .iter()
        .map(|source_id| source_id.0.clone())
        .collect::<Vec<_>>();
    source_ids.sort_unstable();
    source_ids
}

fn vector_scan_source_clause(source_ids: Option<&[String]>) -> String {
    source_ids
        .map(|source_ids| {
            format!(
                " AND source_id IN ({})",
                vec!["?"; source_ids.len()].join(", ")
            )
        })
        .unwrap_or_default()
}

fn vector_scan_params(
    profile_id: &EmbeddingProfileId,
    source_ids: Option<&[String]>,
) -> Vec<String> {
    let mut params = Vec::with_capacity(source_ids.map_or(1, |source_ids| source_ids.len() + 1));
    params.push(profile_id.as_str().to_string());
    if let Some(source_ids) = source_ids {
        params.extend(source_ids.iter().cloned());
    }
    params
}

#[derive(Debug, Clone, Copy)]
enum VectorJsonTable {
    ChunkVectors,
    EmbeddingCache,
}

impl VectorJsonTable {
    fn name(self) -> &'static str {
        match self {
            Self::ChunkVectors => "chunk_vectors",
            Self::EmbeddingCache => "embedding_cache",
        }
    }
}

#[derive(Debug, Default)]
struct VectorJsonCleanupScan {
    stats: VectorJsonCleanupTableStats,
    eligible_rowids: Vec<i64>,
}

fn scan_vector_json_cleanup_table(
    conn: &Connection,
    table: VectorJsonTable,
) -> Result<VectorJsonCleanupScan> {
    let mut scan = VectorJsonCleanupScan::default();
    let mut stmt = conn.prepare(&format!(
        "SELECT rowid, length(vector_json), length(vector_blob) FROM {}",
        table.name()
    ))?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, Option<i64>>(1)?.unwrap_or_default(),
            row.get::<_, Option<i64>>(2)?.unwrap_or_default(),
        ))
    })?;

    for row in rows {
        let (rowid, json_len, blob_len) = row?;
        if blob_len > 0 && blob_len % 4 == 0 {
            if json_len > 0 {
                scan.stats.eligible += 1;
                scan.eligible_rowids.push(rowid);
            } else {
                scan.stats.already_clean += 1;
            }
        } else if blob_len > 0 {
            scan.stats.malformed_blob += 1;
        } else if json_len > 0 {
            scan.stats.json_only += 1;
        } else {
            scan.stats.missing_blob += 1;
        }
    }
    Ok(scan)
}

fn clear_vector_json_payloads(
    tx: &Transaction<'_>,
    table: VectorJsonTable,
    rowids: &[i64],
) -> Result<()> {
    if rowids.is_empty() {
        return Ok(());
    }
    let mut stmt = tx.prepare(&format!(
        "UPDATE {} SET vector_json = '' WHERE rowid = ?1",
        table.name()
    ))?;
    for rowid in rowids {
        stmt.execute(params![rowid])?;
    }
    Ok(())
}

fn decode_stored_vector(
    label: &str,
    vector_blob: Option<&[u8]>,
    vector_json: Option<&str>,
) -> Result<Vec<f32>> {
    if let Some(blob) = vector_blob.filter(|blob| !blob.is_empty()) {
        match blob_to_vector(blob) {
            Ok(vector) => return Ok(vector),
            Err(blob_error) => {
                if let Some(json) = non_empty_vector_json(vector_json) {
                    return parse_stored_vector(label, json).with_context(|| {
                        format!("parse JSON fallback after malformed BLOB for {label}")
                    });
                }
                return Err(blob_error).with_context(|| {
                    format!("decode vector BLOB for {label}; no JSON fallback is present")
                });
            }
        }
    }

    let json = non_empty_vector_json(vector_json)
        .ok_or_else(|| anyhow::anyhow!("{label} has neither vector_blob nor vector_json"))?;
    parse_stored_vector(label, json)
}

fn non_empty_vector_json(vector_json: Option<&str>) -> Option<&str> {
    vector_json.filter(|json| !json.is_empty())
}

fn parse_stored_vector(label: &str, vector_json: &str) -> Result<Vec<f32>> {
    serde_json::from_str(vector_json).with_context(|| format!("parse stored vector for {label}"))
}

/// Serialize a vector as a compact little-endian f32 BLOB.
fn vector_to_blob(vector: &[f32]) -> Vec<u8> {
    let mut bytes = Vec::with_capacity(vector.len() * 4);
    for &val in vector {
        bytes.extend_from_slice(&val.to_le_bytes());
    }
    bytes
}

/// Deserialize a little-endian f32 BLOB back into a Vec<f32>.
fn blob_to_vector(blob: &[u8]) -> Result<Vec<f32>> {
    if !blob.len().is_multiple_of(4) {
        anyhow::bail!("vector BLOB length {} is not a multiple of 4", blob.len());
    }
    let mut result = Vec::with_capacity(blob.len() / 4);
    for chunk in blob.chunks_exact(4) {
        // chunks_exact(4) guarantees 4-byte slices; the is_multiple_of
        // guard above already verified blob.len() is divisible by 4.
        let bytes: [u8; 4] = match chunk.try_into() {
            Ok(arr) => arr,
            Err(_) => continue, // unreachable given guards above
        };
        result.push(f32::from_le_bytes(bytes));
    }
    Ok(result)
}

fn vector_search_score(query: &[f32], vector: &[f32]) -> f32 {
    score_from_squared_l2_distance(
        query
            .iter()
            .zip(vector.iter())
            .map(|(left, right)| (left - right) * (left - right))
            .sum::<f32>(),
    )
}

fn vector_blob_search_score(query: &[f32], blob: &[u8]) -> Result<f32> {
    Ok(score_from_squared_l2_distance(
        vector_blob_squared_l2_distance(query, blob)?,
    ))
}

fn score_from_squared_l2_distance(distance_squared: f32) -> f32 {
    1.0 / (1.0 + distance_squared.sqrt())
}

fn vector_blob_squared_l2_distance(query: &[f32], blob: &[u8]) -> Result<f32> {
    if !blob.len().is_multiple_of(4) {
        anyhow::bail!("vector BLOB length {} is not a multiple of 4", blob.len());
    }
    let vector_len = blob.len() / 4;
    let len = query.len().min(vector_len);

    #[cfg(all(target_arch = "x86_64", target_endian = "little"))]
    {
        Ok(vector_blob_squared_l2_distance_x86_64_sse(query, blob, len))
    }

    #[cfg(not(all(target_arch = "x86_64", target_endian = "little")))]
    {
        Ok(vector_blob_squared_l2_distance_scalar(query, blob, len))
    }
}

fn vector_blob_squared_l2_distance_scalar(query: &[f32], blob: &[u8], len: usize) -> f32 {
    query
        .iter()
        .take(len)
        .zip(blob.chunks_exact(4))
        .map(|(left, bytes)| {
            let right = f32::from_le_bytes([bytes[0], bytes[1], bytes[2], bytes[3]]);
            (left - right) * (left - right)
        })
        .sum()
}

#[cfg(all(target_arch = "x86_64", target_endian = "little"))]
fn vector_blob_squared_l2_distance_x86_64_sse(query: &[f32], blob: &[u8], len: usize) -> f32 {
    use std::arch::x86_64::{_mm_loadu_ps, _mm_mul_ps, _mm_storeu_ps, _mm_sub_ps};

    let simd_len = len - (len % 4);
    let mut distance_squared = 0.0f32;
    let mut lane_squares = [0.0f32; 4];
    let mut index = 0usize;

    while index < simd_len {
        // SAFETY: `len` is capped to both `query.len()` and `blob.len() / 4`.
        // `_mm_loadu_ps` accepts unaligned pointers, every f32 bit pattern is
        // valid, and this function is only compiled on little-endian x86_64 so
        // the BLOB's little-endian f32 bytes match native lane layout.
        unsafe {
            let query_lanes = _mm_loadu_ps(query.as_ptr().add(index));
            let vector_lanes = _mm_loadu_ps(blob.as_ptr().add(index * 4).cast::<f32>());
            let diff = _mm_sub_ps(query_lanes, vector_lanes);
            let squares = _mm_mul_ps(diff, diff);
            _mm_storeu_ps(lane_squares.as_mut_ptr(), squares);
        }
        distance_squared += lane_squares[0];
        distance_squared += lane_squares[1];
        distance_squared += lane_squares[2];
        distance_squared += lane_squares[3];
        index += 4;
    }

    distance_squared
        + vector_blob_squared_l2_distance_scalar(&query[index..], &blob[index * 4..], len - index)
}

fn push_top_vector_hit(
    hits: &mut Vec<(ChunkId, f32)>,
    top_k: usize,
    chunk_id: ChunkId,
    score: f32,
) {
    let candidate = (chunk_id, score);
    if hits.len() < top_k {
        hits.push(candidate);
        return;
    }

    let Some((worst_index, worst_hit)) = hits.iter().enumerate().min_by(|(_, left), (_, right)| {
        left.1
            .partial_cmp(&right.1)
            .unwrap_or(Ordering::Equal)
            .then_with(|| right.0 .0.cmp(&left.0 .0))
    }) else {
        return;
    };
    if is_better_vector_hit(&candidate, worst_hit) {
        hits[worst_index] = candidate;
    }
}

fn is_better_vector_hit(left: &(ChunkId, f32), right: &(ChunkId, f32)) -> bool {
    left.1
        .partial_cmp(&right.1)
        .unwrap_or(Ordering::Equal)
        .then_with(|| right.0 .0.cmp(&left.0 .0))
        == Ordering::Greater
}

fn replace_vector_documents_for_profile_tx(
    tx: &Transaction<'_>,
    profile_id: &EmbeddingProfileId,
    vectors: &[VectorDocument],
) -> Result<()> {
    tx.execute(
        "DELETE FROM chunk_vectors WHERE profile_id = ?1",
        params![profile_id.as_str()],
    )
    .with_context(|| format!("clear chunk vectors for embedding profile {profile_id}"))?;
    insert_vector_documents_for_profile_tx(tx, profile_id, vectors)
}

fn replace_source_vector_documents_for_profile_tx(
    tx: &Transaction<'_>,
    profile_id: &EmbeddingProfileId,
    source_id: &SourceId,
    vectors: &[VectorDocument],
) -> Result<()> {
    tx.execute(
        "DELETE FROM chunk_vectors WHERE profile_id = ?1 AND source_id = ?2",
        params![profile_id.as_str(), &source_id.0],
    )
    .with_context(|| {
        format!(
            "clear chunk vectors for embedding profile {profile_id} and source {}",
            source_id.0
        )
    })?;
    insert_vector_documents_for_profile_tx(tx, profile_id, vectors)
}

fn insert_vector_documents_for_profile_tx(
    tx: &Transaction<'_>,
    profile_id: &EmbeddingProfileId,
    vectors: &[VectorDocument],
) -> Result<()> {
    let mut stmt = tx
        .prepare(
            "INSERT INTO chunk_vectors (profile_id, chunk_id, source_id, vector_json, vector_blob)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )
        .context("prepare vector insert")?;
    for vector in vectors {
        let vector_blob = vector_to_blob(&vector.vector);
        stmt.execute(params![
            profile_id.as_str(),
            &vector.chunk_id.0,
            &vector.source_id.0,
            "",
            vector_blob,
        ])
        .with_context(|| format!("insert vector for chunk {}", vector.chunk_id.0))?;
    }
    Ok(())
}

fn set_source_embedding_status_tx(
    tx: &Transaction<'_>,
    profile_id: &EmbeddingProfileId,
    source_id: &SourceId,
    status: SourceEmbeddingStatus,
    vector_count: usize,
    error_message: Option<&str>,
) -> Result<()> {
    let now = unix_timestamp_string();
    tx.execute(
        "INSERT INTO source_embedding_status
            (profile_id, source_id, status, vector_count, embedded_at, error_message, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?5)
         ON CONFLICT(profile_id, source_id) DO UPDATE SET
            status = excluded.status,
            vector_count = excluded.vector_count,
            embedded_at = excluded.embedded_at,
            error_message = excluded.error_message,
            updated_at = excluded.updated_at",
        params![
            profile_id.as_str(),
            &source_id.0,
            status.as_str(),
            sql_usize(vector_count),
            &now,
            error_message,
        ],
    )?;
    Ok(())
}

fn set_source_embedding_failures_tx(
    tx: &Transaction<'_>,
    profile_id: &EmbeddingProfileId,
    source_vector_counts: &[(SourceId, usize)],
    error_message: &str,
) -> Result<()> {
    for (source_id, vector_count) in source_vector_counts {
        set_source_embedding_status_tx(
            tx,
            profile_id,
            source_id,
            SourceEmbeddingStatus::Failed,
            *vector_count,
            Some(error_message),
        )?;
    }
    Ok(())
}

fn profile_index_generation(conn: &Connection, profile_id: &EmbeddingProfileId) -> Result<u64> {
    let value: Option<String> = conn
        .query_row(
            "SELECT generation FROM embedding_profile_index_meta WHERE profile_id = ?1",
            params![profile_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    value
        .as_deref()
        .unwrap_or("0")
        .parse::<u64>()
        .context("parse profile index generation")
}

fn bump_profile_index_generation(
    tx: &Transaction<'_>,
    profile_id: &EmbeddingProfileId,
) -> Result<u64> {
    tx.execute(
        "INSERT OR IGNORE INTO embedding_profile_index_meta (profile_id, generation)
         VALUES (?1, '0')",
        params![profile_id.as_str()],
    )?;
    let current: Option<String> = tx
        .query_row(
            "SELECT generation FROM embedding_profile_index_meta WHERE profile_id = ?1",
            params![profile_id.as_str()],
            |row| row.get(0),
        )
        .optional()?;
    let next = current
        .as_deref()
        .unwrap_or("0")
        .parse::<u64>()
        .context("parse current profile index generation")?
        .saturating_add(1);
    tx.execute(
        "INSERT OR REPLACE INTO embedding_profile_index_meta (profile_id, generation)
         VALUES (?1, ?2)",
        params![profile_id.as_str(), next.to_string()],
    )?;
    Ok(next)
}

fn bump_all_profile_index_generations(tx: &Transaction<'_>) -> Result<u64> {
    tx.execute(
        "INSERT OR IGNORE INTO embedding_profile_index_meta (profile_id, generation)
         SELECT id, '0' FROM embedding_profiles",
        [],
    )?;
    tx.execute(
        "UPDATE embedding_profile_index_meta
         SET generation = CAST(generation AS INTEGER) + 1",
        [],
    )?;
    let default_generation: String = tx.query_row(
        "SELECT generation FROM embedding_profile_index_meta WHERE profile_id = ?1",
        params![DEFAULT_EMBEDDING_PROFILE_ID],
        |row| row.get(0),
    )?;
    default_generation
        .parse::<u64>()
        .context("parse default profile index generation")
}

fn status_to_str(s: &SourceStatus) -> &'static str {
    match s {
        SourceStatus::Pending => "Pending",
        SourceStatus::Indexed => "Indexed",
        SourceStatus::Stale => "Stale",
    }
}

fn str_to_status(s: &str) -> SourceStatus {
    match s {
        "Indexed" => SourceStatus::Indexed,
        "Stale" => SourceStatus::Stale,
        _ => SourceStatus::Pending,
    }
}

fn evidence_kind_to_str(kind: EvidenceKind) -> &'static str {
    match kind {
        EvidenceKind::Text => "Text",
        EvidenceKind::Ocr => "Ocr",
        EvidenceKind::Image => "Image",
        EvidenceKind::Generated => "Generated",
    }
}

fn str_to_evidence_kind(kind: &str) -> EvidenceKind {
    match kind {
        "Ocr" => EvidenceKind::Ocr,
        "Image" => EvidenceKind::Image,
        "Generated" => EvidenceKind::Generated,
        _ => EvidenceKind::Text,
    }
}

fn image_caption_status_to_str(status: ImageCaptionStatus) -> &'static str {
    match status {
        ImageCaptionStatus::Success => "Success",
        ImageCaptionStatus::Failed => "Failed",
        ImageCaptionStatus::Skipped => "Skipped",
    }
}

fn str_to_image_caption_status(status: &str) -> ImageCaptionStatus {
    match status {
        "Success" => ImageCaptionStatus::Success,
        "Skipped" => ImageCaptionStatus::Skipped,
        _ => ImageCaptionStatus::Failed,
    }
}

fn unix_timestamp_string() -> String {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_secs().to_string())
        .unwrap_or_else(|_| "0".to_string())
}

fn unix_timestamp_millis() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default()
}

fn elapsed_ms_since_unix_seconds(started_at: &str) -> Option<u64> {
    let started_ms = started_at.parse::<u128>().ok()?.saturating_mul(1000);
    unix_timestamp_millis()
        .saturating_sub(started_ms)
        .try_into()
        .ok()
}

fn chunk_type_to_str(ct: &ChunkType) -> &'static str {
    match ct {
        ChunkType::Child => "Child",
        ChunkType::Parent => "Parent",
    }
}

fn str_to_chunk_type(s: &str) -> ChunkType {
    match s {
        "Parent" => ChunkType::Parent,
        _ => ChunkType::Child,
    }
}

fn str_to_graph_node_kind(value: &str, column: usize) -> rusqlite::Result<GraphNodeKind> {
    match value {
        "Source" => Ok(GraphNodeKind::Source),
        "Page" => Ok(GraphNodeKind::Page),
        "Section" => Ok(GraphNodeKind::Section),
        "Chunk" => Ok(GraphNodeKind::Chunk),
        "EvidenceUnit" => Ok(GraphNodeKind::EvidenceUnit),
        "ImageArtifact" => Ok(GraphNodeKind::ImageArtifact),
        "GeneratedEntity" => Ok(GraphNodeKind::GeneratedEntity),
        "GeneratedClaim" => Ok(GraphNodeKind::GeneratedClaim),
        _ => Err(invalid_text_value(
            column,
            format!("unknown graph node kind: {value}"),
        )),
    }
}

fn str_to_edge_type(value: &str, column: usize) -> rusqlite::Result<EdgeType> {
    match value {
        "contains" | "Contains" => Ok(EdgeType::Contains),
        "derived_from" | "DerivedFrom" => Ok(EdgeType::DerivedFrom),
        "parent" => Ok(EdgeType::Parent),
        "child" => Ok(EdgeType::Child),
        "previous" => Ok(EdgeType::Previous),
        "next" | "Next" => Ok(EdgeType::Next),
        "same_source" => Ok(EdgeType::SameSource),
        "same_page" => Ok(EdgeType::SamePage),
        "section_contains" => Ok(EdgeType::SectionContains),
        "page_contains_image" => Ok(EdgeType::PageContainsImage),
        "image_near_text" => Ok(EdgeType::ImageNearText),
        "markdown_links_to" => Ok(EdgeType::MarkdownLinksTo),
        "generated_depends_on" => Ok(EdgeType::GeneratedDependsOn),
        "generated_implements" => Ok(EdgeType::GeneratedImplements),
        "generated_mentions" => Ok(EdgeType::GeneratedMentions),
        "generated_conflicts_with" => Ok(EdgeType::GeneratedConflictsWith),
        "generated_supports" => Ok(EdgeType::GeneratedSupports),
        "generated_other" => Ok(EdgeType::GeneratedOther),
        _ => Err(invalid_text_value(
            column,
            format!("unknown graph edge type: {value}"),
        )),
    }
}

fn invalid_text_value(column: usize, message: String) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        column,
        Type::Text,
        Box::new(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            message,
        )),
    )
}

fn collection_record_from_row(row: &Row<'_>) -> rusqlite::Result<CollectionRecord> {
    let ignore_patterns_json: String = row.get(1)?;
    let last_sync_json: Option<String> = row.get(5)?;
    Ok(CollectionRecord {
        name: row.get(0)?,
        ignore_patterns: json_from_sql(1, &ignore_patterns_json)?,
        watch_enabled: row.get(6)?,
        auto_index_enabled: row.get(7)?,
        created_at: row.get(2)?,
        updated_at: row.get(3)?,
        last_synced_at: row.get(4)?,
        last_sync: last_sync_json
            .as_deref()
            .map(|value| json_from_sql(5, value))
            .transpose()?,
    })
}

fn collection_root_from_row(row: &Row<'_>) -> rusqlite::Result<CollectionRoot> {
    let canonical_path: Option<String> = row.get(2)?;
    let kind: String = row.get(3)?;
    Ok(CollectionRoot {
        collection_name: row.get(0)?,
        path: PathBuf::from(row.get::<_, String>(1)?),
        canonical_path: canonical_path.map(PathBuf::from),
        kind: CollectionRootKind::from_storage_str(&kind),
        added_at: row.get(4)?,
        updated_at: row.get(5)?,
    })
}

fn collection_member_from_row(row: &Row<'_>) -> rusqlite::Result<CollectionMember> {
    Ok(CollectionMember {
        collection_name: row.get(0)?,
        source_id: SourceId(row.get(1)?),
        logical_path: row.get(2)?,
        source_path: PathBuf::from(row.get::<_, String>(3)?),
        updated_at: row.get(4)?,
    })
}

fn json_from_sql<T>(column: usize, value: &str) -> rusqlite::Result<T>
where
    T: DeserializeOwned,
{
    serde_json::from_str(value).map_err(|error| invalid_text_value(column, error.to_string()))
}

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sources (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    hash TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'Pending',
    parser_used TEXT,
    last_ingested_at TEXT
);
CREATE TABLE IF NOT EXISTS collections (
    name TEXT PRIMARY KEY,
    ignore_patterns_json TEXT NOT NULL,
    watch_enabled INTEGER NOT NULL DEFAULT 0,
    auto_index_enabled INTEGER NOT NULL DEFAULT 1,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    last_synced_at TEXT,
    last_sync_json TEXT
);
CREATE TABLE IF NOT EXISTS collection_roots (
    collection_name TEXT NOT NULL REFERENCES collections(name) ON DELETE CASCADE,
    path TEXT NOT NULL,
    canonical_path TEXT,
    kind TEXT NOT NULL,
    added_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (collection_name, path)
);
CREATE INDEX IF NOT EXISTS collection_roots_collection_idx
    ON collection_roots(collection_name);
CREATE TABLE IF NOT EXISTS collection_members (
    collection_name TEXT NOT NULL REFERENCES collections(name) ON DELETE CASCADE,
    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    logical_path TEXT NOT NULL,
    source_path TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (collection_name, source_id)
);
CREATE INDEX IF NOT EXISTS collection_members_collection_idx
    ON collection_members(collection_name);
CREATE INDEX IF NOT EXISTS collection_members_source_idx
    ON collection_members(source_id);
CREATE TABLE IF NOT EXISTS evidence_units (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    kind TEXT NOT NULL DEFAULT 'Text',
    locator_json TEXT NOT NULL,
    text TEXT NOT NULL,
    text_hash TEXT NOT NULL,
    heading_path_json TEXT,
    position INTEGER NOT NULL,
    derived_from_evidence_id TEXT
);
CREATE TABLE IF NOT EXISTS image_artifacts (
    image_id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    evidence_unit_id TEXT NOT NULL REFERENCES evidence_units(id) ON DELETE CASCADE,
    relative_path TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    mime_type TEXT NOT NULL,
    width INTEGER NOT NULL,
    height INTEGER NOT NULL,
    page INTEGER NOT NULL,
    image_index INTEGER NOT NULL,
    bbox_json TEXT
);
CREATE TABLE IF NOT EXISTS image_captions (
    image_hash TEXT NOT NULL,
    model TEXT NOT NULL,
    prompt_version TEXT NOT NULL,
    prompt_hash TEXT NOT NULL,
    status TEXT NOT NULL,
    caption_json TEXT,
    raw_response TEXT,
    error_message TEXT,
    attempt_count INTEGER NOT NULL,
    cache_hits INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (image_hash, model, prompt_hash)
);
CREATE TABLE IF NOT EXISTS chunks (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    chunk_hash TEXT NOT NULL DEFAULT '',
    embedding_input_hash TEXT,
    text TEXT NOT NULL,
    context_text TEXT,
    token_count INTEGER NOT NULL,
    chunk_type TEXT NOT NULL,
    parent_chunk_id TEXT REFERENCES chunks(id),
    heading_path_json TEXT
);
CREATE TABLE IF NOT EXISTS chunk_evidence (
    chunk_id TEXT NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
    evidence_unit_id TEXT NOT NULL REFERENCES evidence_units(id) ON DELETE CASCADE,
    PRIMARY KEY (chunk_id, evidence_unit_id)
);
CREATE TABLE IF NOT EXISTS chunk_evidence_spans (
    chunk_id TEXT NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
    ordinal INTEGER NOT NULL,
    evidence_unit_id TEXT NOT NULL REFERENCES evidence_units(id) ON DELETE CASCADE,
    chunk_byte_start INTEGER NOT NULL,
    chunk_byte_end INTEGER NOT NULL,
    evidence_byte_start INTEGER NOT NULL,
    evidence_byte_end INTEGER NOT NULL,
    evidence_text_hash TEXT NOT NULL,
    locator_json TEXT NOT NULL,
    trust_json TEXT NOT NULL,
    PRIMARY KEY (chunk_id, ordinal)
);
CREATE TABLE IF NOT EXISTS embedding_profiles (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    dimension INTEGER NOT NULL,
    normalize INTEGER NOT NULL,
    endpoint_identity TEXT,
    requested_model TEXT,
    served_model TEXT,
    max_context_tokens INTEGER,
    dtype TEXT,
    quantization TEXT,
    weight_identity TEXT,
    chunker_version TEXT NOT NULL DEFAULT 'parent-child-v3',
    child_target_tokens INTEGER NOT NULL DEFAULT 300,
    child_overlap_tokens INTEGER NOT NULL DEFAULT 80,
    parent_children_count INTEGER NOT NULL DEFAULT 5,
    embedding_input_budget_tokens INTEGER,
    query_instruction_hash TEXT NOT NULL DEFAULT '',
    document_instruction_hash TEXT NOT NULL DEFAULT '',
    config_hash TEXT NOT NULL,
    qdrant_collection TEXT,
    qdrant_vector_name TEXT,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS chunk_vectors (
    profile_id TEXT NOT NULL REFERENCES embedding_profiles(id) ON DELETE CASCADE,
    chunk_id TEXT NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    vector_json TEXT NOT NULL,
    PRIMARY KEY (profile_id, chunk_id)
);
CREATE TABLE IF NOT EXISTS embedding_cache (
    profile_id TEXT NOT NULL REFERENCES embedding_profiles(id) ON DELETE CASCADE,
    profile_config_hash TEXT NOT NULL,
    embedding_input_hash TEXT NOT NULL,
    vector_json TEXT NOT NULL,
    dimension INTEGER NOT NULL,
    cache_hits INTEGER NOT NULL DEFAULT 0,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (profile_id, profile_config_hash, embedding_input_hash)
);
CREATE TABLE IF NOT EXISTS source_embedding_status (
    profile_id TEXT NOT NULL REFERENCES embedding_profiles(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    status TEXT NOT NULL,
    vector_count INTEGER NOT NULL,
    embedded_at TEXT,
    error_message TEXT,
    updated_at TEXT NOT NULL,
    PRIMARY KEY (profile_id, source_id)
);
CREATE TABLE IF NOT EXISTS graph_nodes (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    external_id TEXT NOT NULL,
    label TEXT,
    locator_json TEXT,
    ordinal INTEGER,
    metadata_json TEXT,
    UNIQUE(source_id, kind, external_id)
);
CREATE TABLE IF NOT EXISTS graph_edges (
    id TEXT PRIMARY KEY,
    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    edge_type TEXT NOT NULL,
    from_node_id TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
    to_node_id TEXT NOT NULL REFERENCES graph_nodes(id) ON DELETE CASCADE,
    ordinal INTEGER,
    weight REAL,
    metadata_json TEXT,
    UNIQUE(source_id, edge_type, from_node_id, to_node_id, ordinal)
);
CREATE INDEX IF NOT EXISTS graph_nodes_source_idx
    ON graph_nodes(source_id);
CREATE INDEX IF NOT EXISTS graph_nodes_kind_external_idx
    ON graph_nodes(source_id, kind, external_id);
CREATE INDEX IF NOT EXISTS graph_edges_source_idx
    ON graph_edges(source_id);
CREATE INDEX IF NOT EXISTS graph_edges_from_idx
    ON graph_edges(from_node_id);
CREATE INDEX IF NOT EXISTS graph_edges_to_idx
    ON graph_edges(to_node_id);
CREATE INDEX IF NOT EXISTS graph_edges_type_idx
    ON graph_edges(source_id, edge_type);
CREATE TABLE IF NOT EXISTS tasks (
    id TEXT PRIMARY KEY,
    kind TEXT NOT NULL,
    status TEXT NOT NULL,
    created_at TEXT NOT NULL,
    updated_at TEXT NOT NULL,
    started_at TEXT,
    finished_at TEXT,
    request_json TEXT NOT NULL,
    result_json TEXT,
    error TEXT,
    progress_json TEXT,
    progress_phase TEXT,
    progress_wait_reason TEXT,
    progress_recent_status TEXT,
    progress_phase_started_at TEXT,
    profile_json TEXT
);
CREATE INDEX IF NOT EXISTS tasks_status_updated_idx
    ON tasks(status, updated_at);
CREATE TABLE IF NOT EXISTS task_events (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    event_type TEXT NOT NULL,
    message TEXT NOT NULL,
    payload_json TEXT NOT NULL,
    created_at TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS task_events_task_id_idx
    ON task_events(task_id, id);
CREATE TABLE IF NOT EXISTS task_spans (
    id INTEGER PRIMARY KEY AUTOINCREMENT,
    task_id TEXT NOT NULL REFERENCES tasks(id) ON DELETE CASCADE,
    phase TEXT NOT NULL,
    started_at TEXT NOT NULL,
    duration_ms INTEGER NOT NULL,
    metadata_json TEXT NOT NULL
);
CREATE INDEX IF NOT EXISTS task_spans_task_id_idx
    ON task_spans(task_id, id);
CREATE INDEX IF NOT EXISTS task_spans_phase_idx
    ON task_spans(phase);
CREATE TABLE IF NOT EXISTS embeddings_meta (
    profile_id TEXT NOT NULL REFERENCES embedding_profiles(id) ON DELETE CASCADE,
    chunk_id TEXT NOT NULL REFERENCES chunks(id) ON DELETE CASCADE,
    hnsw_position INTEGER NOT NULL,
    embedding_model TEXT NOT NULL,
    embedded_at TEXT NOT NULL,
    PRIMARY KEY (profile_id, chunk_id)
);
CREATE TABLE IF NOT EXISTS index_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS embedding_profile_index_meta (
    profile_id TEXT PRIMARY KEY REFERENCES embedding_profiles(id) ON DELETE CASCADE,
    generation TEXT NOT NULL
);
CREATE VIRTUAL TABLE IF NOT EXISTS chunk_fts USING fts5(
    chunk_id UNINDEXED,
    source_id UNINDEXED,
    text,
    heading
);
CREATE TRIGGER IF NOT EXISTS chunks_ai_fts
AFTER INSERT ON chunks
WHEN NEW.chunk_type = 'Child'
BEGIN
    INSERT INTO chunk_fts(rowid, chunk_id, source_id, text, heading)
    VALUES (
        NEW.rowid,
        NEW.id,
        NEW.source_id,
        CASE
            WHEN NEW.context_text IS NULL OR NEW.context_text = '' THEN NEW.text
            ELSE NEW.context_text || ' ' || NEW.text
        END,
        COALESCE(NEW.heading_path_json, '')
    );
END;
CREATE TRIGGER IF NOT EXISTS chunks_ad_fts
AFTER DELETE ON chunks
WHEN OLD.chunk_type = 'Child'
BEGIN
    DELETE FROM chunk_fts WHERE rowid = OLD.rowid;
END;
CREATE TRIGGER IF NOT EXISTS chunks_au_delete_fts
AFTER UPDATE ON chunks
WHEN OLD.chunk_type = 'Child'
BEGIN
    DELETE FROM chunk_fts WHERE rowid = OLD.rowid;
END;
CREATE TRIGGER IF NOT EXISTS chunks_au_insert_fts
AFTER UPDATE ON chunks
WHEN NEW.chunk_type = 'Child'
BEGIN
    INSERT INTO chunk_fts(rowid, chunk_id, source_id, text, heading)
    VALUES (
        NEW.rowid,
        NEW.id,
        NEW.source_id,
        CASE
            WHEN NEW.context_text IS NULL OR NEW.context_text = '' THEN NEW.text
            ELSE NEW.context_text || ' ' || NEW.text
        END,
        COALESCE(NEW.heading_path_json, '')
    );
END;
"#;

#[cfg(test)]
mod tests {
    use super::*;
    use crate::chunker::CHUNKER_VERSION;
    use crate::task::{
        ask_request_metadata, ask_result_metadata, ingest_request_metadata,
        ingest_task_request_metadata_with_queue_claim, PhaseTiming, TASK_EVENT_MESSAGE_MAX_CHARS,
    };
    use crate::types::{BBox, SourceLocator};
    use std::path::PathBuf;
    use tempfile::tempdir;

    pub(super) fn sample_source() -> Source {
        Source {
            id: SourceId("src-1".into()),
            path: PathBuf::from("/tmp/test.pdf"),
            hash: "abc123".into(),
            status: SourceStatus::Pending,
            parser_used: Some("pdf_oxide".into()),
            last_ingested_at: None,
        }
    }

    #[test]
    fn store_new_applies_wal_journal_mode() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test_wal.db");
        {
            let store = Store::new(&db_path).unwrap();
            let mode: String = store
                .conn
                .query_row("PRAGMA journal_mode", [], |row| row.get(0))
                .unwrap();
            assert_eq!(
                mode.to_lowercase(),
                "wal",
                "Store::new should set journal_mode=WAL"
            );
            let sync: i64 = store
                .conn
                .query_row("PRAGMA synchronous", [], |row| row.get(0))
                .unwrap();
            assert_eq!(
                sync, 1,
                "Store::new should set synchronous=NORMAL (value 1 in SQLite)"
            );
        }
    }

    #[test]
    fn in_memory_store_uses_full_synchronous_when_wal_unavailable() {
        let store = Store::in_memory().unwrap();
        let mode: String = store
            .conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_ne!(
            mode.to_lowercase(),
            "wal",
            "in-memory databases cannot enable WAL mode"
        );
        let sync: i64 = store
            .conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .unwrap();
        assert_eq!(
            sync, 2,
            "Store::in_memory should keep synchronous=FULL when WAL is unavailable"
        );
    }

    #[test]
    fn vector_blob_roundtrip_preserves_values() {
        // Verify vector_to_blob → blob_to_vector round-trips exactly.
        let vector = vec![1.5f32, -2.3, 0.0, 42.0, f32::INFINITY, f32::NEG_INFINITY];
        let blob = vector_to_blob(&vector);
        let recovered = blob_to_vector(&blob).unwrap();
        assert_eq!(vector, recovered);
    }

    #[test]
    fn vector_blob_search_score_matches_vector_score() {
        let query = vec![0.5f32, -1.0, 2.0, 8.0, -3.5, 0.25, 7.0, 1.0, 9.0];
        let vector = vec![1.5f32, -2.3, 0.0, 42.0, -4.0, 0.5, 6.5, 1.25, 10.0];
        let blob = vector_to_blob(&vector);

        assert_eq!(
            vector_blob_search_score(&query, &blob).unwrap(),
            vector_search_score(&query, &vector)
        );

        let short_query = vec![0.5f32, -1.0, 2.0];
        assert_eq!(
            vector_blob_search_score(&short_query, &blob).unwrap(),
            vector_search_score(&short_query, &vector)
        );
        assert!(vector_blob_search_score(&query, &[1, 2, 3])
            .unwrap_err()
            .to_string()
            .contains("multiple of 4"));
    }

    #[test]
    fn low_memory_vector_search_uses_blob_without_parsing_json() {
        let store = Store::in_memory().unwrap();
        let profile = EmbeddingProfileId::default_profile();
        store
            .conn
            .execute(
                "INSERT INTO sources (id, path, hash, status, parser_used, last_ingested_at)
                 VALUES ('s1', '/tmp/x', 'h', 'Indexed', 'test', NULL)",
                [],
            )
            .unwrap();
        store.conn.execute(
            "INSERT INTO chunks (id, source_id, text, context_text, token_count, chunk_type, parent_chunk_id, heading_path_json)
             VALUES ('c1', 's1', 'text', NULL, 1, 'Leaf', NULL, '[]')",
            [],
        ).unwrap();
        store
            .replace_all_vector_documents_for_profile(
                &profile,
                &[VectorDocument {
                    chunk_id: ChunkId("c1".into()),
                    source_id: SourceId("s1".into()),
                    vector: vec![1.0f32, 0.0, 0.0, 0.0],
                }],
            )
            .unwrap();
        store
            .conn
            .execute("UPDATE chunk_vectors SET vector_json = 'not json'", [])
            .unwrap();

        let hits = store
            .search_vector_documents_for_profile(&profile, &[1.0, 0.0, 0.0, 0.0], 1, None)
            .unwrap();

        assert_eq!(hits, vec![(ChunkId("c1".into()), 1.0)]);
    }

    #[test]
    fn vector_blob_migration_backfills_existing_json_vectors() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("test_blob_migrate.db");
        // Create store and insert a vector.
        {
            let store = Store::new(&db_path).unwrap();
            let profile = EmbeddingProfileId::default_profile();
            // Insert prerequisite source + chunk rows.
            store
                .conn
                .execute(
                    "INSERT INTO sources (id, path, hash, status, parser_used, last_ingested_at)
                 VALUES ('s1', '/tmp/x', 'h', 'Indexed', 'test', NULL)",
                    [],
                )
                .unwrap();
            store.conn.execute(
                "INSERT INTO chunks (id, source_id, text, context_text, token_count, chunk_type, parent_chunk_id, heading_path_json)
                 VALUES ('c1', 's1', 'text', NULL, 1, 'Leaf', NULL, '[]')",
                [],
            ).unwrap();
            let vectors = vec![VectorDocument {
                chunk_id: ChunkId("c1".into()),
                source_id: SourceId("s1".into()),
                vector: vec![1.0f32, 2.0, 3.0],
            }];
            store
                .replace_all_vector_documents_for_profile(&profile, &vectors)
                .unwrap();
            store
                .conn
                .execute(
                    "UPDATE chunk_vectors SET vector_json = '[1.0,2.0,3.0]' WHERE chunk_id = 'c1'",
                    [],
                )
                .unwrap();
        }
        // Wipe vector_blob, then re-open (triggers migration backfill).
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute("UPDATE chunk_vectors SET vector_blob = NULL", [])
                .unwrap();
        }
        let store = Store::new(&db_path).unwrap();
        let profile = EmbeddingProfileId::default_profile();
        let docs = store.list_vector_documents_for_profile(&profile).unwrap();
        assert_eq!(docs.len(), 1);
        assert_eq!(docs[0].vector, vec![1.0f32, 2.0, 3.0]);
    }

    #[test]
    fn chunk_vector_writes_are_blob_only_and_readable() {
        let store = Store::in_memory().unwrap();
        let profile = EmbeddingProfileId::default_profile();
        insert_vector_test_source_and_chunks(&store, "s1", &["c1"]);

        store
            .replace_all_vector_documents_for_profile(
                &profile,
                &[VectorDocument {
                    chunk_id: ChunkId("c1".into()),
                    source_id: SourceId("s1".into()),
                    vector: vec![1.0, 2.0],
                }],
            )
            .unwrap();

        let (vector_json, blob_len): (String, i64) = store
            .conn
            .query_row(
                "SELECT vector_json, length(vector_blob) FROM chunk_vectors WHERE chunk_id = 'c1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(vector_json, "");
        assert_eq!(blob_len, 8);
        assert_eq!(
            store.list_vector_documents_for_profile(&profile).unwrap()[0].vector,
            vec![1.0, 2.0]
        );
    }

    #[test]
    fn legacy_json_only_chunk_vectors_remain_readable_and_searchable() {
        let store = Store::in_memory().unwrap();
        let profile = EmbeddingProfileId::default_profile();
        insert_vector_test_source_and_chunks(&store, "s1", &["c-json"]);
        insert_chunk_vector_row(&store, "s1", "c-json", &[3.0, 4.0], None);

        assert_eq!(
            store.list_vector_documents_for_profile(&profile).unwrap()[0].vector,
            vec![3.0, 4.0]
        );
        let hits = store
            .search_vector_documents_for_profile(&profile, &[3.0, 4.0], 1, None)
            .unwrap();
        assert_eq!(hits[0].0, ChunkId("c-json".into()));
    }

    #[test]
    fn malformed_chunk_vector_blob_uses_json_fallback() {
        let store = Store::in_memory().unwrap();
        let profile = EmbeddingProfileId::default_profile();
        insert_vector_test_source_and_chunks(&store, "s1", &["c-malformed"]);
        insert_chunk_vector_row(
            &store,
            "s1",
            "c-malformed",
            &[5.0, 6.0],
            Some(vec![1, 2, 3]),
        );

        assert_eq!(
            store.list_vector_documents_for_profile(&profile).unwrap()[0].vector,
            vec![5.0, 6.0]
        );
        let hits = store
            .search_vector_documents_for_profile(&profile, &[5.0, 6.0], 1, None)
            .unwrap();
        assert_eq!(hits[0].0, ChunkId("c-malformed".into()));
    }

    #[test]
    fn embedding_cache_writes_are_blob_only_and_hits_remain_readable() {
        let store = Store::in_memory().unwrap();
        let profile = EmbeddingProfileId::default_profile();
        store
            .upsert_embedding_cache_entries(
                &profile,
                "config-a",
                &[EmbeddingCacheEntry {
                    embedding_input_hash: "input-a".into(),
                    vector: vec![7.0, 8.0],
                }],
            )
            .unwrap();

        let (vector_json, blob_len): (String, i64) = store
            .conn
            .query_row(
                "SELECT vector_json, length(vector_blob) FROM embedding_cache WHERE embedding_input_hash = 'input-a'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(vector_json, "");
        assert_eq!(blob_len, 8);
        assert_eq!(
            store
                .get_embedding_cache_vector(&profile, "config-a", "input-a")
                .unwrap()
                .unwrap(),
            vec![7.0, 8.0]
        );
        store
            .record_embedding_cache_hit(&profile, "config-a", "input-a")
            .unwrap();
        let cache_hits: i64 = store
            .conn
            .query_row(
                "SELECT cache_hits FROM embedding_cache WHERE embedding_input_hash = 'input-a'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(cache_hits, 1);
    }

    #[test]
    fn legacy_json_only_embedding_cache_remains_readable() {
        let store = Store::in_memory().unwrap();
        let profile = EmbeddingProfileId::default_profile();
        insert_embedding_cache_row(&store, "config-a", "input-json", &[9.0, 10.0], None);

        assert_eq!(
            store
                .get_embedding_cache_vector(&profile, "config-a", "input-json")
                .unwrap()
                .unwrap(),
            vec![9.0, 10.0]
        );
    }

    #[test]
    fn malformed_embedding_cache_blob_uses_json_fallback() {
        let store = Store::in_memory().unwrap();
        let profile = EmbeddingProfileId::default_profile();
        insert_embedding_cache_row(
            &store,
            "config-a",
            "input-malformed",
            &[11.0, 12.0],
            Some(vec![1, 2, 3]),
        );

        assert_eq!(
            store
                .get_embedding_cache_vector(&profile, "config-a", "input-malformed")
                .unwrap()
                .unwrap(),
            vec![11.0, 12.0]
        );
    }

    #[test]
    fn vector_json_cleanup_reports_and_clears_only_eligible_rows() {
        let store = Store::in_memory().unwrap();
        insert_vector_test_source_and_chunks(
            &store,
            "s1",
            &[
                "chunk-eligible",
                "chunk-clean",
                "chunk-json-only",
                "chunk-missing",
                "chunk-malformed",
            ],
        );
        insert_chunk_vector_row(
            &store,
            "s1",
            "chunk-eligible",
            &[1.0, 0.0],
            Some(vector_to_blob(&[1.0, 0.0])),
        );
        insert_chunk_vector_row(
            &store,
            "s1",
            "chunk-clean",
            &[],
            Some(vector_to_blob(&[1.0, 0.0])),
        );
        store
            .conn
            .execute(
                "UPDATE chunk_vectors SET vector_json = '' WHERE chunk_id = 'chunk-clean'",
                [],
            )
            .unwrap();
        insert_chunk_vector_row(&store, "s1", "chunk-json-only", &[2.0, 0.0], None);
        store
            .conn
            .execute(
                "INSERT INTO chunk_vectors (profile_id, chunk_id, source_id, vector_json, vector_blob)
                 VALUES ('default', 'chunk-missing', 's1', '', NULL)",
                [],
            )
            .unwrap();
        insert_chunk_vector_row(
            &store,
            "s1",
            "chunk-malformed",
            &[3.0, 0.0],
            Some(vec![1, 2, 3]),
        );

        insert_embedding_cache_row(
            &store,
            "config-a",
            "cache-eligible",
            &[1.0, 0.0],
            Some(vector_to_blob(&[1.0, 0.0])),
        );
        insert_embedding_cache_row(
            &store,
            "config-a",
            "cache-clean",
            &[],
            Some(vector_to_blob(&[1.0, 0.0])),
        );
        store
            .conn
            .execute(
                "UPDATE embedding_cache SET vector_json = '' WHERE embedding_input_hash = 'cache-clean'",
                [],
            )
            .unwrap();
        insert_embedding_cache_row(&store, "config-a", "cache-json-only", &[2.0, 0.0], None);
        store
            .conn
            .execute(
                "INSERT INTO embedding_cache
                    (profile_id, profile_config_hash, embedding_input_hash, vector_json, vector_blob, dimension, cache_hits, created_at, updated_at)
                 VALUES ('default', 'config-a', 'cache-missing', '', NULL, 2, 0, '1', '1')",
                [],
            )
            .unwrap();
        insert_embedding_cache_row(
            &store,
            "config-a",
            "cache-malformed",
            &[3.0, 0.0],
            Some(vec![1, 2, 3]),
        );

        let dry_run = store.vector_json_cleanup_dry_run().unwrap();
        assert_eq!(
            dry_run.tables.chunk_vectors,
            VectorJsonCleanupTableStats {
                eligible: 1,
                already_clean: 1,
                json_only: 1,
                missing_blob: 1,
                malformed_blob: 1,
            }
        );
        assert_eq!(dry_run.tables.embedding_cache, dry_run.tables.chunk_vectors);
        assert_eq!(dry_run.cleared, VectorJsonCleanupCleared::default());
        assert_ne!(chunk_vector_json(&store, "chunk-eligible"), "");

        let applied = store.cleanup_vector_json_payloads().unwrap();
        assert_eq!(applied.tables, dry_run.tables);
        assert_eq!(
            applied.cleared,
            VectorJsonCleanupCleared {
                chunk_vectors: 1,
                embedding_cache: 1,
            }
        );
        assert_eq!(chunk_vector_json(&store, "chunk-eligible"), "");
        assert_ne!(chunk_vector_json(&store, "chunk-json-only"), "");
        assert_ne!(chunk_vector_json(&store, "chunk-malformed"), "");
        assert_eq!(embedding_cache_json(&store, "cache-eligible"), "");
        assert_ne!(embedding_cache_json(&store, "cache-json-only"), "");
        assert_ne!(embedding_cache_json(&store, "cache-malformed"), "");
    }

    #[test]
    fn vector_json_cleanup_preserves_readable_fallback_rows() {
        let store = Store::in_memory().unwrap();
        let profile = EmbeddingProfileId::default_profile();
        insert_vector_test_source_and_chunks(
            &store,
            "s1",
            &["chunk-eligible", "chunk-json-only", "chunk-malformed"],
        );
        insert_chunk_vector_row(
            &store,
            "s1",
            "chunk-eligible",
            &[1.0, 0.0],
            Some(vector_to_blob(&[1.0, 0.0])),
        );
        insert_chunk_vector_row(&store, "s1", "chunk-json-only", &[2.0, 0.0], None);
        insert_chunk_vector_row(
            &store,
            "s1",
            "chunk-malformed",
            &[3.0, 0.0],
            Some(vec![1, 2, 3]),
        );
        insert_embedding_cache_row(
            &store,
            "config-a",
            "cache-eligible",
            &[1.0, 0.0],
            Some(vector_to_blob(&[1.0, 0.0])),
        );
        insert_embedding_cache_row(&store, "config-a", "cache-json-only", &[2.0, 0.0], None);
        insert_embedding_cache_row(
            &store,
            "config-a",
            "cache-malformed",
            &[3.0, 0.0],
            Some(vec![1, 2, 3]),
        );

        store.cleanup_vector_json_payloads().unwrap();

        let docs = store.list_vector_documents_for_profile(&profile).unwrap();
        assert_eq!(docs.len(), 3);
        let hits = store
            .search_vector_documents_for_profile(&profile, &[3.0, 0.0], 3, None)
            .unwrap();
        assert_eq!(hits[0].0, ChunkId("chunk-malformed".into()));
        assert_eq!(
            store
                .get_embedding_cache_vector(&profile, "config-a", "cache-eligible")
                .unwrap()
                .unwrap(),
            vec![1.0, 0.0]
        );
        assert_eq!(
            store
                .get_embedding_cache_vector(&profile, "config-a", "cache-json-only")
                .unwrap()
                .unwrap(),
            vec![2.0, 0.0]
        );
        assert_eq!(
            store
                .get_embedding_cache_vector(&profile, "config-a", "cache-malformed")
                .unwrap()
                .unwrap(),
            vec![3.0, 0.0]
        );
    }

    #[test]
    fn vector_json_cleanup_rolls_back_on_failure() {
        let store = Store::in_memory().unwrap();
        insert_vector_test_source_and_chunks(&store, "s1", &["chunk-a", "chunk-b"]);
        insert_chunk_vector_row(
            &store,
            "s1",
            "chunk-a",
            &[1.0, 0.0],
            Some(vector_to_blob(&[1.0, 0.0])),
        );
        insert_chunk_vector_row(
            &store,
            "s1",
            "chunk-b",
            &[2.0, 0.0],
            Some(vector_to_blob(&[2.0, 0.0])),
        );
        insert_embedding_cache_row(
            &store,
            "config-a",
            "cache-a",
            &[1.0, 0.0],
            Some(vector_to_blob(&[1.0, 0.0])),
        );
        store
            .conn
            .execute_batch(
                "
                CREATE TRIGGER fail_vector_json_cleanup
                BEFORE UPDATE OF vector_json ON chunk_vectors
                WHEN OLD.chunk_id = 'chunk-b'
                BEGIN
                    SELECT RAISE(FAIL, 'forced vector JSON cleanup failure');
                END;
                ",
            )
            .unwrap();

        let error = store.cleanup_vector_json_payloads().unwrap_err();
        assert!(error
            .to_string()
            .contains("forced vector JSON cleanup failure"));
        assert_ne!(chunk_vector_json(&store, "chunk-a"), "");
        assert_ne!(chunk_vector_json(&store, "chunk-b"), "");
        assert_ne!(embedding_cache_json(&store, "cache-a"), "");
    }

    #[test]
    fn readonly_open_existing_does_not_run_migrations() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("legacy.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE tasks (
                    id TEXT PRIMARY KEY,
                    kind TEXT NOT NULL,
                    status TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    started_at TEXT,
                    finished_at TEXT,
                    request_json TEXT NOT NULL,
                    result_json TEXT,
                    error TEXT
                );",
            )
            .unwrap();
        }

        let readonly = Store::open_existing_readonly(&db_path).unwrap();
        assert!(!table_has_column(readonly.connection(), "tasks", "progress_json").unwrap());
        drop(readonly);

        let migrated = Store::new(&db_path).unwrap();
        assert!(table_has_column(migrated.connection(), "tasks", "progress_json").unwrap());
        assert!(table_has_column(migrated.connection(), "tasks", "profile_json").unwrap());
    }

    #[test]
    fn legacy_task_profile_migration_is_idempotent_and_preserves_rows() {
        let dir = tempdir().unwrap();
        let db_path = dir.path().join("legacy-profile.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "CREATE TABLE tasks (
                    id TEXT PRIMARY KEY,
                    kind TEXT NOT NULL,
                    status TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    started_at TEXT,
                    finished_at TEXT,
                    request_json TEXT NOT NULL,
                    result_json TEXT,
                    error TEXT
                );
                INSERT INTO tasks (
                    id, kind, status, created_at, updated_at, started_at, finished_at,
                    request_json, result_json, error
                ) VALUES (
                    'task-legacy', 'retrieve', 'succeeded', '1', '2', '1', '2',
                    '{\"question\":\"old\"}', '{\"returned_results\":0}', NULL
                );
                INSERT INTO tasks (
                    id, kind, status, created_at, updated_at, started_at, finished_at,
                    request_json, result_json, error
                ) VALUES (
                    'task-running', 'retrieve', 'running', '3', '4', '4', NULL,
                    '{\"question\":\"new\"}', NULL, NULL
                );",
            )
            .unwrap();
        }

        let running_id = TaskId("task-running".into());
        let profile = crate::task::TaskProfile {
            schema_version: crate::task::TASK_PROFILE_SCHEMA_VERSION,
            task_id: running_id.clone(),
            task_kind: TaskKind::Retrieve,
            status: TaskStatus::Succeeded,
            queue_wait_ms: 1,
            total_wall_ms: 9,
            controls: Default::default(),
            resources: Default::default(),
            endpoints: Vec::new(),
            retrieve: None,
            ask: None,
        };
        {
            let migrated = Store::new(&db_path).unwrap();
            assert!(table_has_column(migrated.connection(), "tasks", "profile_json").unwrap());
            assert!(migrated
                .get_task_profile(&TaskId("task-legacy".into()))
                .unwrap()
                .is_none());
            assert!(migrated
                .finish_task_success_with_profile(
                    &running_id,
                    &crate::task::retrieve_result_metadata(0, 0, false),
                    &profile,
                )
                .unwrap());
        }

        let reopened = Store::new(&db_path).unwrap();
        let legacy = reopened
            .get_task(&TaskId("task-legacy".into()))
            .unwrap()
            .expect("legacy row should survive migrations");
        assert_eq!(legacy.status, TaskStatus::Succeeded);
        assert!(reopened.get_task_profile(&legacy.id).unwrap().is_none());
        let stored_profile = reopened
            .get_task_profile(&running_id)
            .unwrap()
            .expect("profile data should survive repeated migration");
        assert_eq!(stored_profile, profile);
    }

    pub(super) fn sample_evidence(source_id: &str) -> Vec<EvidenceUnit> {
        vec![
            EvidenceUnit {
                id: EvidenceId("ev-1".into()),
                source_id: SourceId(source_id.into()),
                kind: EvidenceKind::Text,
                derived_from: None,
                locator: SourceLocator::Pdf {
                    page: 1,
                    paragraph: 1,
                    bbox: None,
                },
                text: "First paragraph.".into(),
                text_hash: "h1".into(),
                heading_path: vec!["Chapter 1".into()],
                position: 0,
            },
            EvidenceUnit {
                id: EvidenceId("ev-2".into()),
                source_id: SourceId(source_id.into()),
                kind: EvidenceKind::Text,
                derived_from: None,
                locator: SourceLocator::Pdf {
                    page: 1,
                    paragraph: 2,
                    bbox: None,
                },
                text: "Second paragraph.".into(),
                text_hash: "h2".into(),
                heading_path: vec!["Chapter 1".into()],
                position: 1,
            },
        ]
    }

    pub(super) fn sample_chunks(source_id: &str) -> Vec<Chunk> {
        vec![
            Chunk {
                id: ChunkId("parent-1".into()),
                source_id: SourceId(source_id.into()),
                chunk_hash: "hash-parent-1".into(),
                embedding_input_hash: None,
                text: "First paragraph. Second paragraph.".into(),
                context_text: None,
                token_count: 50,
                chunk_type: ChunkType::Parent,
                parent_chunk_id: None,
                heading_path: vec!["Chapter 1".into()],
                evidence_unit_ids: vec![],
            },
            Chunk {
                id: ChunkId("child-1".into()),
                source_id: SourceId(source_id.into()),
                chunk_hash: "hash-child-1".into(),
                embedding_input_hash: Some("embedding-hash-child-1".into()),
                text: "First paragraph.".into(),
                context_text: Some("This chunk is from Chapter 1.".into()),
                token_count: 20,
                chunk_type: ChunkType::Child,
                parent_chunk_id: Some(ChunkId("parent-1".into())),
                heading_path: vec!["Chapter 1".into()],
                evidence_unit_ids: vec![],
            },
        ]
    }

    fn insert_vector_test_source_and_chunks(store: &Store, source_id: &str, chunk_ids: &[&str]) {
        store
            .conn
            .execute(
                "INSERT INTO sources (id, path, hash, status, parser_used, last_ingested_at)
                 VALUES (?1, ?2, 'h', 'Indexed', 'test', NULL)",
                params![source_id, format!("/tmp/{source_id}.md")],
            )
            .unwrap();
        for chunk_id in chunk_ids {
            store
                .conn
                .execute(
                    "INSERT INTO chunks (id, source_id, chunk_hash, embedding_input_hash, text, context_text, token_count, chunk_type, parent_chunk_id, heading_path_json)
                     VALUES (?1, ?2, ?3, ?4, 'text', NULL, 1, 'Leaf', NULL, '[]')",
                    params![
                        chunk_id,
                        source_id,
                        format!("hash-{chunk_id}"),
                        format!("embedding-{chunk_id}"),
                    ],
                )
                .unwrap();
        }
    }

    fn insert_chunk_vector_row(
        store: &Store,
        source_id: &str,
        chunk_id: &str,
        vector: &[f32],
        vector_blob: Option<Vec<u8>>,
    ) {
        let vector_json = serde_json::to_string(vector).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO chunk_vectors (profile_id, chunk_id, source_id, vector_json, vector_blob)
                 VALUES ('default', ?1, ?2, ?3, ?4)",
                params![chunk_id, source_id, vector_json, vector_blob],
            )
            .unwrap();
    }

    fn insert_embedding_cache_row(
        store: &Store,
        profile_config_hash: &str,
        embedding_input_hash: &str,
        vector: &[f32],
        vector_blob: Option<Vec<u8>>,
    ) {
        let vector_json = serde_json::to_string(vector).unwrap();
        store
            .conn
            .execute(
                "INSERT INTO embedding_cache
                    (profile_id, profile_config_hash, embedding_input_hash, vector_json, vector_blob, dimension, cache_hits, created_at, updated_at)
                 VALUES ('default', ?1, ?2, ?3, ?4, ?5, 0, '1', '1')",
                params![
                    profile_config_hash,
                    embedding_input_hash,
                    vector_json,
                    vector_blob,
                    sql_usize(vector.len()),
                ],
            )
            .unwrap();
    }

    fn chunk_vector_json(store: &Store, chunk_id: &str) -> String {
        store
            .conn
            .query_row(
                "SELECT vector_json FROM chunk_vectors WHERE chunk_id = ?1",
                params![chunk_id],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn embedding_cache_json(store: &Store, embedding_input_hash: &str) -> String {
        store
            .conn
            .query_row(
                "SELECT vector_json FROM embedding_cache WHERE embedding_input_hash = ?1",
                params![embedding_input_hash],
                |row| row.get(0),
            )
            .unwrap()
    }

    fn sample_image_artifact(source_id: &str, evidence_id: &str) -> ImageArtifact {
        ImageArtifact {
            image_id: ImageId("src-1:img:abc".into()),
            source_id: SourceId(source_id.into()),
            evidence_id: EvidenceId(evidence_id.into()),
            relative_path: PathBuf::from("image-artifacts/src-1/src-1-img-abc.png"),
            content_hash: "image-hash".into(),
            mime_type: "image/png".into(),
            width: 16,
            height: 8,
            page: 1,
            image_index: 1,
            bbox: Some(BBox {
                x0: 1.0,
                y0: 2.0,
                x1: 3.0,
                y1: 4.0,
            }),
        }
    }

    pub(super) fn profile_id(id: &str) -> EmbeddingProfileId {
        EmbeddingProfileId::new(id).unwrap()
    }

    pub(super) fn test_profile_config<'a>(
        provider: &'a str,
        model: &'a str,
        dimension: usize,
        normalize: bool,
        query_instruction: &'a str,
        document_instruction: &'a str,
    ) -> EmbeddingProfileConfig<'a> {
        EmbeddingProfileConfig {
            provider,
            model,
            dimension,
            normalize,
            endpoint_identity: None,
            requested_model: None,
            served_model: None,
            max_context_tokens: None,
            dtype: None,
            quantization: None,
            weight_identity: None,
            chunker_version: CHUNKER_VERSION,
            child_target_tokens: 300,
            child_overlap_tokens: 80,
            parent_children_count: 5,
            embedding_input_budget_tokens: None,
            query_instruction,
            document_instruction,
        }
    }

    fn ensure_test_profile(store: &Store, profile_id: &EmbeddingProfileId) {
        store
            .ensure_embedding_profile(
                profile_id,
                test_profile_config("test", profile_id.as_str(), 2, true, "", ""),
            )
            .unwrap();
    }

    fn empty_collection_report() -> CollectionSyncReport {
        CollectionSyncReport {
            member_count: 0,
            added: 0,
            removed: 0,
            unchanged: 0,
            scanned_roots: 1,
            max_depth: 32,
            skipped: Vec::new(),
        }
    }

    #[test]
    fn collection_memberships_are_materialized_and_source_shared() {
        let dir = tempdir().unwrap();
        let source_path = dir.path().join("article.md");
        std::fs::write(&source_path, "article").unwrap();
        let canonical_source_path = std::fs::canonicalize(&source_path).unwrap();
        let source_id = SourceId::from_path(&canonical_source_path);
        let store = Store::in_memory().unwrap();
        store
            .add_source(&Source {
                id: source_id.clone(),
                path: canonical_source_path.clone(),
                hash: "hash".into(),
                status: SourceStatus::Pending,
                parser_used: None,
                last_ingested_at: None,
            })
            .unwrap();

        store
            .create_collection("articles", &["drafts/".to_string()])
            .unwrap();
        let collection = store.get_collection("articles").unwrap().unwrap();
        assert!(!collection.watch_enabled);
        assert!(collection.auto_index_enabled);
        let collection = store
            .update_collection_watch_settings("articles", true, false)
            .unwrap()
            .unwrap();
        assert!(collection.watch_enabled);
        assert!(!collection.auto_index_enabled);
        store.create_collection("areskapitalon", &[]).unwrap();
        store.add_collection_root("articles", dir.path()).unwrap();

        let candidate = CollectionMemberCandidate {
            source_id: source_id.clone(),
            logical_path: "article.md".into(),
            source_path: canonical_source_path,
        };
        let articles_report = store
            .replace_collection_members(
                "articles",
                std::slice::from_ref(&candidate),
                empty_collection_report(),
            )
            .unwrap();
        store
            .replace_collection_members("areskapitalon", &[candidate], empty_collection_report())
            .unwrap();

        assert_eq!(articles_report.added, 1);
        assert_eq!(
            store.list_collection_members("articles").unwrap()[0].source_id,
            source_id
        );
        assert_eq!(
            store.list_collection_members("areskapitalon").unwrap()[0].source_id,
            source_id
        );
        let collection_members = store
            .list_collection_members_for_collections(&[
                "articles".to_string(),
                "areskapitalon".to_string(),
            ])
            .unwrap();
        assert_eq!(collection_members.len(), 2);
        let unique_source_ids = collection_members
            .iter()
            .map(|member| member.source_id.clone())
            .collect::<std::collections::BTreeSet<_>>();
        assert_eq!(unique_source_ids.len(), 1);

        assert!(store.delete_collection("articles").unwrap());
        assert!(store.get_source(&source_id).unwrap().is_some());
        assert!(store
            .list_collection_members("articles")
            .unwrap()
            .is_empty());
        assert_eq!(
            store
                .list_collection_members("areskapitalon")
                .unwrap()
                .len(),
            1
        );

        store.remove_source(&source_id).unwrap();
        assert!(store
            .list_collection_members("areskapitalon")
            .unwrap()
            .is_empty());
    }

    #[test]
    fn legacy_single_profile_vectors_migrate_to_default_profile() {
        let tempdir = tempdir().unwrap();
        let db_path = tempdir.path().join("legacy.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "
                CREATE TABLE sources (
                    id TEXT PRIMARY KEY,
                    path TEXT NOT NULL,
                    hash TEXT NOT NULL,
                    status TEXT NOT NULL,
                    parser_used TEXT,
                    last_ingested_at TEXT
                );
                CREATE TABLE chunks (
                    id TEXT PRIMARY KEY,
                    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
                    text TEXT NOT NULL,
                    context_text TEXT,
                    token_count INTEGER NOT NULL,
                    chunk_type TEXT NOT NULL,
                    parent_chunk_id TEXT REFERENCES chunks(id),
                    heading_path_json TEXT
                );
                CREATE TABLE chunk_vectors (
                    chunk_id TEXT PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
                    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
                    vector_json TEXT NOT NULL
                );
                CREATE TABLE embeddings_meta (
                    chunk_id TEXT PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
                    hnsw_position INTEGER NOT NULL,
                    embedding_model TEXT NOT NULL,
                    embedded_at TEXT NOT NULL
                );
                CREATE TABLE index_meta (
                    key TEXT PRIMARY KEY,
                    value TEXT NOT NULL
                );
                INSERT INTO sources (id, path, hash, status, parser_used, last_ingested_at)
                    VALUES ('src-1', '/tmp/test.pdf', 'abc', 'Indexed', 'pdf_oxide', NULL);
                INSERT INTO chunks (id, source_id, text, context_text, token_count, chunk_type, parent_chunk_id, heading_path_json)
                    VALUES ('child-1', 'src-1', 'text', NULL, 1, 'Child', NULL, '[]');
                INSERT INTO chunk_vectors (chunk_id, source_id, vector_json)
                    VALUES ('child-1', 'src-1', '[1.0,0.0]');
                INSERT INTO embeddings_meta (chunk_id, hnsw_position, embedding_model, embedded_at)
                    VALUES ('child-1', 3, 'legacy-model', '2025-01-01');
                INSERT INTO index_meta (key, value) VALUES ('generation', '7');
                ",
            )
            .unwrap();
        }

        let store = Store::new(&db_path).unwrap();
        let default_profile = EmbeddingProfileId::default_profile();
        let vectors = store
            .list_vector_documents_for_profile(&default_profile)
            .unwrap();
        assert_eq!(vectors.len(), 1);
        assert_eq!(vectors[0].chunk_id.0, "child-1");
        assert_eq!(vectors[0].vector, vec![1.0, 0.0]);
        assert_eq!(
            store
                .get_embedding_meta_for_profile(&default_profile, &ChunkId("child-1".into()))
                .unwrap(),
            Some((3, "legacy-model".into(), "2025-01-01".into()))
        );
        assert_eq!(
            store
                .index_generation_for_profile(&default_profile)
                .unwrap(),
            7
        );

        store
            .ensure_embedding_profile(
                &default_profile,
                test_profile_config(
                    "custom",
                    "custom-model",
                    384,
                    false,
                    "query instruction",
                    "document instruction",
                ),
            )
            .unwrap();
    }

    #[test]
    fn legacy_tasks_without_progress_metadata_columns_migrate_before_index_creation() {
        let tempdir = tempdir().unwrap();
        let db_path = tempdir.path().join("legacy-tasks.db");
        let progress = TaskProgressSnapshot::phase(IngestTaskStage::EmbeddingQueueWait.as_str())
            .with_wait_reason("embedding_batch")
            .with_recent_status("waiting for embedding batch");
        let progress_started_at = progress.phase.as_ref().unwrap().started_at.clone();
        let progress_json = serde_json::to_string(&progress).unwrap();
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "
                CREATE TABLE tasks (
                    id TEXT PRIMARY KEY,
                    kind TEXT NOT NULL,
                    status TEXT NOT NULL,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL,
                    started_at TEXT,
                    finished_at TEXT,
                    request_json TEXT NOT NULL,
                    result_json TEXT,
                    error TEXT,
                    progress_json TEXT
                );
                CREATE INDEX tasks_status_updated_idx
                    ON tasks(status, updated_at);
                ",
            )
            .unwrap();
            conn.execute(
                "INSERT INTO tasks
                    (id, kind, status, created_at, updated_at, started_at, finished_at, request_json, result_json, error, progress_json)
                 VALUES (?1, ?2, ?3, '1', '2', '1', NULL, '{}', NULL, NULL, ?4)",
                params![
                    "task-pre-160",
                    TaskKind::Ingest.as_str(),
                    TaskStatus::Running.as_str(),
                    progress_json,
                ],
            )
            .unwrap();
        }

        let store = Store::new(&db_path).unwrap();
        for column in [
            "progress_phase",
            "progress_wait_reason",
            "progress_recent_status",
            "progress_phase_started_at",
        ] {
            assert!(table_has_column(store.connection(), "tasks", column).unwrap());
        }
        let migrated_metadata = store
            .connection()
            .query_row(
                "SELECT progress_phase, progress_wait_reason, progress_recent_status, progress_phase_started_at
                 FROM tasks WHERE id = 'task-pre-160'",
                [],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                        row.get::<_, Option<String>>(2)?,
                        row.get::<_, Option<String>>(3)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            migrated_metadata,
            (
                Some(IngestTaskStage::EmbeddingQueueWait.as_str().to_string()),
                Some("embedding_batch".to_string()),
                Some("waiting for embedding batch".to_string()),
                Some(progress_started_at),
            )
        );
        let index_exists = store
            .connection()
            .query_row(
                "SELECT EXISTS(
                    SELECT 1 FROM sqlite_master
                    WHERE type = 'index' AND name = 'tasks_active_progress_metadata_idx'
                )",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap();
        assert_eq!(index_exists, 1);

        let summary = store
            .get_task(&TaskId("task-pre-160".into()))
            .unwrap()
            .unwrap();
        assert_eq!(summary.status, TaskStatus::Running);
        assert_eq!(
            summary
                .progress
                .and_then(|progress| progress.phase)
                .map(|phase| phase.name),
            Some(IngestTaskStage::EmbeddingQueueWait.as_str().to_string())
        );
        assert_eq!(
            store
                .active_task_metadata_aggregate(5)
                .unwrap()
                .embedding_waiting,
            1
        );
    }

    #[test]
    fn embedding_profile_rejects_changed_document_instruction() {
        let store = Store::in_memory().unwrap();
        let profile = profile_id("instructional");
        store
            .ensure_embedding_profile(
                &profile,
                test_profile_config(
                    "test",
                    "model",
                    2,
                    true,
                    "same query",
                    "document instruction v1",
                ),
            )
            .unwrap();

        let query_err = store
            .ensure_embedding_profile(
                &profile,
                test_profile_config(
                    "test",
                    "model",
                    2,
                    true,
                    "changed query",
                    "document instruction v1",
                ),
            )
            .unwrap_err();
        assert!(query_err.to_string().contains("query_instruction_hash"));

        let err = store
            .ensure_embedding_profile(
                &profile,
                test_profile_config(
                    "test",
                    "model",
                    2,
                    true,
                    "same query",
                    "document instruction v2",
                ),
            )
            .unwrap_err();

        assert!(err
            .to_string()
            .contains("embedding profile 'instructional' already exists"));
        assert!(err.to_string().contains("document_instruction_hash"));
    }

    #[test]
    fn embedding_profile_migration_preserves_nonlegacy_config_compatibility() {
        let tempdir = tempdir().unwrap();
        let db_path = tempdir.path().join("profiles.db");
        {
            let conn = Connection::open(&db_path).unwrap();
            conn.execute_batch(
                "
                CREATE TABLE embedding_profiles (
                    id TEXT PRIMARY KEY,
                    provider TEXT NOT NULL,
                    model TEXT NOT NULL,
                    dimension INTEGER NOT NULL,
                    normalize INTEGER NOT NULL,
                    config_hash TEXT NOT NULL,
                    qdrant_collection TEXT,
                    qdrant_vector_name TEXT,
                    created_at TEXT NOT NULL,
                    updated_at TEXT NOT NULL
                );
                INSERT INTO embedding_profiles
                    (id, provider, model, dimension, normalize, config_hash, qdrant_collection, qdrant_vector_name, created_at, updated_at)
                    VALUES ('custom', 'provider-a', 'model-a', 2, 1, 'pre-instruction-config', NULL, NULL, '2026-01-01', '2026-01-01');
                ",
            )
            .unwrap();
        }

        let store = Store::new(&db_path).unwrap();
        let profile = profile_id("custom");
        let err = store
            .ensure_embedding_profile(
                &profile,
                test_profile_config("provider-b", "model-a", 2, true, "", ""),
            )
            .unwrap_err();
        assert!(err
            .to_string()
            .contains("embedding profile 'custom' already exists"));

        store
            .ensure_embedding_profile(
                &profile,
                test_profile_config("provider-a", "model-a", 2, true, "", ""),
            )
            .unwrap();
    }

    #[test]
    fn embedding_profile_reset_rolls_back_profile_update_when_clear_fails() {
        let store = Store::in_memory().unwrap();
        let profile = profile_id("atomic-reset");
        let source = sample_source();
        let chunk_id = ChunkId("child-1".into());
        let first_config = EmbeddingProfileConfig {
            served_model: Some("Served-A"),
            ..test_profile_config("test", "model", 2, true, "", "")
        };
        let second_config = EmbeddingProfileConfig {
            served_model: Some("Served-B"),
            ..test_profile_config("test", "model", 2, true, "", "")
        };
        let first_config_hash = first_config.config_hash();

        store
            .ensure_embedding_profile(&profile, first_config)
            .unwrap();
        store.add_source(&source).unwrap();
        store
            .bulk_insert_chunks(&sample_chunks(&source.id.0))
            .unwrap();
        store
            .replace_all_vector_documents_for_profile(
                &profile,
                &[VectorDocument {
                    chunk_id: chunk_id.clone(),
                    source_id: source.id.clone(),
                    vector: vec![1.0, 0.0],
                }],
            )
            .unwrap();
        store
            .set_source_embedding_status(
                &profile,
                &source.id,
                SourceEmbeddingStatus::Embedded,
                1,
                None,
            )
            .unwrap();
        store
            .set_embedding_meta_for_profile(&profile, &chunk_id, 0, "model", "2026-01-01")
            .unwrap();
        store
            .upsert_embedding_cache_entries(
                &profile,
                &first_config_hash,
                &[EmbeddingCacheEntry {
                    embedding_input_hash: "input-a".into(),
                    vector: vec![1.0, 0.0],
                }],
            )
            .unwrap();
        let first_generation = store.index_generation_for_profile(&profile).unwrap();

        store
            .conn
            .execute_batch(
                "
                CREATE TRIGGER fail_profile_vector_reset
                BEFORE DELETE ON chunk_vectors
                WHEN OLD.profile_id = 'atomic-reset'
                BEGIN
                    SELECT RAISE(FAIL, 'forced profile reset failure');
                END;
                ",
            )
            .unwrap();

        let err = store
            .ensure_embedding_profile(&profile, second_config)
            .unwrap_err();
        assert!(err.to_string().contains("forced profile reset failure"));
        let stored = store
            .load_embedding_profile_config(&profile)
            .unwrap()
            .unwrap();
        assert_eq!(stored.config_hash, first_config_hash);
        assert_eq!(stored.served_model.as_deref(), Some("Served-A"));
        assert_eq!(
            store
                .list_vector_documents_for_profile(&profile)
                .unwrap()
                .len(),
            1
        );
        assert!(store
            .get_embedding_cache_vector(&profile, &first_config_hash, "input-a")
            .unwrap()
            .is_some());
        assert!(store
            .get_embedding_meta_for_profile(&profile, &chunk_id)
            .unwrap()
            .is_some());
        assert!(!store
            .source_vectors_stale_for_profile(&profile, &source.id)
            .unwrap());
        assert_eq!(
            store.index_generation_for_profile(&profile).unwrap(),
            first_generation
        );

        store
            .conn
            .execute_batch("DROP TRIGGER fail_profile_vector_reset;")
            .unwrap();
        assert!(store
            .ensure_embedding_profile(&profile, second_config)
            .unwrap());
        assert!(store
            .list_vector_documents_for_profile(&profile)
            .unwrap()
            .is_empty());
        assert!(store
            .get_embedding_cache_vector(&profile, &first_config_hash, "input-a")
            .unwrap()
            .is_none());
        assert!(store
            .get_embedding_meta_for_profile(&profile, &chunk_id)
            .unwrap()
            .is_none());
        assert!(store
            .source_vectors_stale_for_profile(&profile, &source.id)
            .unwrap());
        assert_eq!(
            store.index_generation_for_profile(&profile).unwrap(),
            first_generation + 1
        );
    }

    #[test]
    fn stores_multiple_profile_vectors_for_same_chunk() {
        let store = Store::in_memory().unwrap();
        store.add_source(&sample_source()).unwrap();
        store.bulk_insert_chunks(&sample_chunks("src-1")).unwrap();
        let default_profile = EmbeddingProfileId::default_profile();
        let alt_profile = profile_id("alt");
        ensure_test_profile(&store, &alt_profile);

        store
            .replace_all_vector_documents_for_profile(
                &default_profile,
                &[VectorDocument {
                    chunk_id: ChunkId("child-1".into()),
                    source_id: SourceId("src-1".into()),
                    vector: vec![1.0, 0.0],
                }],
            )
            .unwrap();
        store
            .replace_all_vector_documents_for_profile(
                &alt_profile,
                &[VectorDocument {
                    chunk_id: ChunkId("child-1".into()),
                    source_id: SourceId("src-1".into()),
                    vector: vec![0.0, 1.0],
                }],
            )
            .unwrap();

        assert_eq!(
            store
                .list_vector_documents_for_profile(&default_profile)
                .unwrap()[0]
                .vector,
            vec![1.0, 0.0]
        );
        assert_eq!(
            store
                .list_vector_documents_for_profile(&alt_profile)
                .unwrap()[0]
                .vector,
            vec![0.0, 1.0]
        );
    }

    #[test]
    fn replacing_one_profile_does_not_delete_another_profiles_vectors() {
        let store = Store::in_memory().unwrap();
        store.add_source(&sample_source()).unwrap();
        store.bulk_insert_chunks(&sample_chunks("src-1")).unwrap();
        let default_profile = EmbeddingProfileId::default_profile();
        let alt_profile = profile_id("alt");
        ensure_test_profile(&store, &alt_profile);

        store
            .replace_all_vector_documents_for_profile(
                &default_profile,
                &[VectorDocument {
                    chunk_id: ChunkId("child-1".into()),
                    source_id: SourceId("src-1".into()),
                    vector: vec![1.0, 0.0],
                }],
            )
            .unwrap();
        store
            .replace_all_vector_documents_for_profile(
                &alt_profile,
                &[VectorDocument {
                    chunk_id: ChunkId("child-1".into()),
                    source_id: SourceId("src-1".into()),
                    vector: vec![0.0, 1.0],
                }],
            )
            .unwrap();

        store
            .replace_all_vector_documents_for_profile(
                &alt_profile,
                &[VectorDocument {
                    chunk_id: ChunkId("child-1".into()),
                    source_id: SourceId("src-1".into()),
                    vector: vec![0.5, 0.5],
                }],
            )
            .unwrap();

        assert_eq!(
            store
                .list_vector_documents_for_profile(&default_profile)
                .unwrap()[0]
                .vector,
            vec![1.0, 0.0]
        );
        assert_eq!(
            store
                .list_vector_documents_for_profile(&alt_profile)
                .unwrap()[0]
                .vector,
            vec![0.5, 0.5]
        );
    }

    fn sample_graph_node(
        source_id: &SourceId,
        kind: GraphNodeKind,
        external_id: &str,
    ) -> GraphNode {
        GraphNode {
            id: GraphNodeId::new(source_id, kind, external_id),
            source_id: source_id.clone(),
            kind,
            external_id: external_id.to_string(),
            label: Some(external_id.to_string()),
            locator: None,
            ordinal: None,
            metadata: None,
        }
    }

    fn sample_graph_edge(
        source_id: &SourceId,
        edge_type: EdgeType,
        from_node_id: &GraphNodeId,
        to_node_id: &GraphNodeId,
        ordinal: Option<u32>,
    ) -> GraphEdge {
        GraphEdge {
            id: GraphEdgeId::new(source_id, edge_type, from_node_id, to_node_id, ordinal),
            source_id: source_id.clone(),
            edge_type,
            from_node_id: from_node_id.clone(),
            to_node_id: to_node_id.clone(),
            ordinal,
            weight: None,
            metadata: None,
        }
    }

    #[test]
    fn graph_schema_tables_exist() {
        let store = Store::in_memory().unwrap();

        store
            .connection()
            .query_row("SELECT COUNT(*) FROM graph_nodes", [], |_| Ok(()))
            .unwrap();
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM graph_edges", [], |_| Ok(()))
            .unwrap();
        store
            .connection()
            .query_row("SELECT COUNT(*) FROM tasks", [], |_| Ok(()))
            .unwrap();
    }

    #[test]
    fn task_summary_events_and_spans_are_persisted_bounded_and_queryable() {
        let store = Store::in_memory().unwrap();
        let task_id = TaskId("task-test".into());
        let request = ask_request_metadata(
            "Do not persist this raw prompt with password=secret.",
            Some("src-1"),
            Some("default"),
            true,
            false,
        );

        let created = store
            .create_task(&task_id, TaskKind::Ask, &request)
            .unwrap();

        assert_eq!(created.status, TaskStatus::Queued);
        assert_eq!(created.kind, TaskKind::Ask);
        assert!(created.request.get("question_sha256").is_some());

        assert!(store.start_task(&task_id).unwrap());
        let progress = store
            .update_task_progress(
                &task_id,
                TaskProgressSnapshot::phase("embedding")
                    .with_counter("vectors", 4, Some(8))
                    .with_endpoint(crate::task::TaskEndpointSummary::single_call(
                        "embedding",
                        25,
                    ))
                    .with_active_worker_kind("ask")
                    .with_recent_status("embedding query"),
            )
            .unwrap()
            .expect("running task accepts progress");
        assert_eq!(progress.counters[0].name, "vectors");

        let event = store
            .insert_task_event(
                &task_id,
                "phase",
                &"x".repeat(TASK_EVENT_MESSAGE_MAX_CHARS + 20),
                &serde_json::json!({"api_key": "should-not-print", "safe": "ok"}),
            )
            .unwrap();
        assert_eq!(event.sequence, 2);
        assert!(event.message.contains("...[truncated]"));
        assert_eq!(event.payload["api_key"], "<redacted>");

        let timing = PhaseTiming::start("chat").finish(serde_json::json!({"model": "qwen"}));
        let span = store
            .insert_task_span(
                &task_id,
                &timing.phase,
                &timing.started_at,
                timing.duration_ms,
                &timing.metadata,
            )
            .unwrap();
        assert_eq!(span.phase, "chat");

        let result = ask_result_metadata("Do not persist this raw answer [E1].", 1, true, false);
        store.finish_task_success(&task_id, &result).unwrap();

        let summary = store.get_task(&task_id).unwrap().unwrap();
        let encoded = serde_json::to_string(&summary).unwrap();
        assert_eq!(summary.status, TaskStatus::Succeeded);
        assert!(summary.started_at.is_some());
        assert!(summary.finished_at.is_some());
        assert!(summary.result.unwrap().get("answer_sha256").is_some());
        assert_eq!(
            summary
                .progress
                .as_ref()
                .and_then(|progress| progress.phase.as_ref())
                .map(|phase| phase.name.as_str()),
            Some("embedding")
        );
        assert!(!encoded.contains("Do not persist this raw prompt"));
        assert!(!encoded.contains("Do not persist this raw answer"));
        assert!(!encoded.contains("should-not-print"));

        let events = store.list_task_events(&task_id, None, 100).unwrap();
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].event_type, "progress");
        assert!(store
            .list_task_events(&task_id, Some(events[1].sequence), 100)
            .unwrap()
            .is_empty());
        let spans = store.list_task_spans(&task_id).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].metadata["model"], "qwen");
    }

    #[test]
    fn completed_retrieve_task_profile_is_persisted_and_queryable() {
        let store = Store::in_memory().unwrap();
        let task_id = TaskId("task-profile".into());
        store
            .create_task(
                &task_id,
                TaskKind::Retrieve,
                &crate::task::retrieve_request_metadata(
                    "What supports profile lookup?",
                    None,
                    None,
                    3,
                    3,
                    1,
                ),
            )
            .unwrap();
        assert!(store.start_task(&task_id).unwrap());

        let profile = crate::task::TaskProfile {
            schema_version: crate::task::TASK_PROFILE_SCHEMA_VERSION,
            task_id: task_id.clone(),
            task_kind: TaskKind::Retrieve,
            status: TaskStatus::Succeeded,
            queue_wait_ms: 0,
            total_wall_ms: 17,
            controls: Default::default(),
            resources: Default::default(),
            endpoints: Vec::new(),
            retrieve: None,
            ask: None,
        };
        store
            .finish_task_success_with_profile(
                &task_id,
                &crate::task::retrieve_result_metadata(2, 1, false),
                &profile,
            )
            .unwrap();

        let stored = store
            .get_task_profile(&task_id)
            .unwrap()
            .expect("retrieve task profile should be stored");
        assert_eq!(stored, profile);
        let summary = store
            .get_task(&task_id)
            .unwrap()
            .expect("task summary should be stored");
        assert_eq!(summary.status, TaskStatus::Succeeded);
        assert!(summary.result.is_some());
        assert!(summary.error.is_none());
    }

    #[test]
    fn malformed_stored_task_profile_returns_error_without_panicking() {
        let store = Store::in_memory().unwrap();
        let task_id = TaskId("task-malformed-profile".into());
        store
            .create_task(
                &task_id,
                TaskKind::Retrieve,
                &crate::task::retrieve_request_metadata(
                    "What happens to malformed profiles?",
                    None,
                    None,
                    3,
                    3,
                    1,
                ),
            )
            .unwrap();
        assert!(store.start_task(&task_id).unwrap());
        assert!(store
            .finish_task_success(
                &task_id,
                &crate::task::retrieve_result_metadata(0, 0, false)
            )
            .unwrap());
        store
            .conn
            .execute(
                "UPDATE tasks SET profile_json = ?2 WHERE id = ?1",
                params![&task_id.0, "{not valid json"],
            )
            .unwrap();

        let error = store.get_task_profile(&task_id).unwrap_err();
        assert!(error.to_string().contains("deserialize task profile"));
    }

    #[test]
    fn cancelled_task_is_not_overwritten_by_late_profile_success() {
        let store = Store::in_memory().unwrap();
        let task_id = TaskId("task-cancel-profile".into());
        store
            .create_task(
                &task_id,
                TaskKind::Retrieve,
                &crate::task::retrieve_request_metadata("cancelled", None, None, 3, 3, 1),
            )
            .unwrap();
        assert!(store.start_task(&task_id).unwrap());
        assert!(store.cancel_task(&task_id).unwrap());

        let profile = crate::task::TaskProfile {
            schema_version: crate::task::TASK_PROFILE_SCHEMA_VERSION,
            task_id: task_id.clone(),
            task_kind: TaskKind::Retrieve,
            status: TaskStatus::Succeeded,
            queue_wait_ms: 0,
            total_wall_ms: 1,
            controls: Default::default(),
            resources: Default::default(),
            endpoints: Vec::new(),
            retrieve: None,
            ask: None,
        };
        assert!(!store
            .finish_task_success_with_profile(
                &task_id,
                &crate::task::retrieve_result_metadata(1, 1, false),
                &profile,
            )
            .unwrap());

        let summary = store.get_task(&task_id).unwrap().unwrap();
        assert_eq!(summary.status, TaskStatus::Cancelled);
        assert!(summary.result.is_none());
        assert!(store.get_task_profile(&task_id).unwrap().is_none());
    }

    #[test]
    fn task_spans_are_capped_per_task() {
        let store = Store::in_memory().unwrap();
        let task_id = TaskId("task-span-cap".into());
        store
            .create_task(&task_id, TaskKind::Ingest, &serde_json::json!({}))
            .unwrap();

        for index in 0..TASK_SPAN_MAX_PER_TASK + 3 {
            store
                .insert_task_span(
                    &task_id,
                    IngestTaskStage::Parse.as_str(),
                    "1",
                    index as u64,
                    &serde_json::json!({"index": index}),
                )
                .unwrap();
        }

        assert_eq!(
            store.list_task_spans(&task_id).unwrap().len(),
            TASK_SPAN_MAX_PER_TASK
        );
    }

    #[test]
    fn task_list_page_is_limited_and_reports_total() {
        let store = Store::in_memory().unwrap();
        for index in 0..8 {
            let task_id = TaskId(format!("task-history-{index:02}"));
            store
                .create_task(
                    &task_id,
                    TaskKind::Ask,
                    &ask_request_metadata("What is cited?", None, None, false, false),
                )
                .unwrap();
            store.start_task(&task_id).unwrap();
            store
                .finish_task_success(&task_id, &ask_result_metadata("answer", 0, false, false))
                .unwrap();
        }
        for index in 0..3 {
            store
                .create_task(
                    &TaskId(format!("task-active-{index:02}")),
                    TaskKind::Ingest,
                    &ingest_request_metadata(Some("src-1"), false),
                )
                .unwrap();
        }

        let history = store.list_tasks_page(TaskListFilter::All, 5).unwrap();
        assert_eq!(history.total, 11);
        assert_eq!(history.tasks.len(), 5);

        let active = store.list_tasks_page(TaskListFilter::Active, 2).unwrap();
        assert_eq!(active.total, 3);
        assert_eq!(active.tasks.len(), 2);
        assert!(active
            .tasks
            .iter()
            .all(|task| task.status == TaskStatus::Queued));
    }

    #[test]
    fn cancelled_task_is_not_overwritten_by_late_success() {
        let store = Store::in_memory().unwrap();
        let task_id = TaskId("task-cancel".into());

        store
            .create_task(
                &task_id,
                TaskKind::Ingest,
                &ingest_request_metadata(Some("src-1"), false),
            )
            .unwrap();
        assert!(store.start_task(&task_id).unwrap());

        assert!(store.cancel_task(&task_id).unwrap());
        store
            .finish_task_success(&task_id, &serde_json::json!({"ingested": 1}))
            .unwrap();

        let summary = store.get_task(&task_id).unwrap().unwrap();
        assert_eq!(summary.status, TaskStatus::Cancelled);
        assert!(summary.result.is_none());
    }

    #[test]
    fn cancelled_queued_task_cannot_start_work_body() {
        let store = Store::in_memory().unwrap();
        let task_id = TaskId("task-cancel-queued".into());

        store
            .create_task(
                &task_id,
                TaskKind::Ingest,
                &ingest_request_metadata(Some("src-1"), false),
            )
            .unwrap();
        assert!(store.cancel_task(&task_id).unwrap());

        let mut work_called = false;
        if store.start_task(&task_id).unwrap() {
            work_called = true;
            store
                .finish_task_success(&task_id, &serde_json::json!({"ingested": 1}))
                .unwrap();
        }

        let summary = store.get_task(&task_id).unwrap().unwrap();
        assert!(!work_called);
        assert_eq!(summary.status, TaskStatus::Cancelled);
        assert!(summary.result.is_none());
    }

    #[test]
    fn failed_task_resume_requeues_without_losing_request_or_events() {
        let store = Store::in_memory().unwrap();
        let task_id = TaskId("task-resume".into());
        let request = ingest_task_request_metadata_with_queue_claim(
            Some("src-1"),
            false,
            Some("profile-a"),
            true,
            true,
        );
        store
            .create_task(&task_id, TaskKind::Ingest, &request)
            .unwrap();
        store.start_task(&task_id).unwrap();
        store
            .insert_task_event(
                &task_id,
                "progress",
                "durable chunks written",
                &serde_json::json!({"chunks": 3}),
            )
            .unwrap();
        store
            .finish_task_failed(&task_id, "embedding provider unavailable")
            .unwrap();

        assert!(store.resume_failed_task(&task_id).unwrap());
        assert!(!store.resume_failed_task(&task_id).unwrap());

        let summary = store.get_task(&task_id).unwrap().unwrap();
        assert_eq!(summary.status, TaskStatus::Queued);
        assert_eq!(summary.request, request);
        assert!(summary.started_at.is_none());
        assert!(summary.finished_at.is_none());
        assert!(summary.error.is_none());
        assert!(summary.result.is_none());
        assert!(summary.progress.is_none());

        let events = store.list_task_events(&task_id, None, 10).unwrap();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].event_type, "progress");
    }

    #[test]
    fn source_crud() {
        let store = Store::in_memory().unwrap();
        let src = sample_source();

        store.add_source(&src).unwrap();
        let listed = store.list_sources().unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].id.0, "src-1");

        let got = store.get_source(&SourceId("src-1".into())).unwrap();
        assert!(got.is_some());
        assert_eq!(got.unwrap().hash, "abc123");

        store
            .update_source_status(&SourceId("src-1".into()), &SourceStatus::Indexed)
            .unwrap();
        let got = store
            .get_source(&SourceId("src-1".into()))
            .unwrap()
            .unwrap();
        assert_eq!(got.status, SourceStatus::Indexed);

        store.remove_source(&SourceId("src-1".into())).unwrap();
        assert!(store.list_sources().unwrap().is_empty());
    }

    #[test]
    fn graph_edge_crud_queries_are_idempotent() {
        let store = Store::in_memory().unwrap();
        let source = sample_source();
        let source_id = source.id.clone();
        store.add_source(&source).unwrap();

        let source_node = sample_graph_node(&source_id, GraphNodeKind::Source, &source_id.0);
        let chunk_node = sample_graph_node(&source_id, GraphNodeKind::Chunk, "chunk-1");
        let evidence_node = sample_graph_node(&source_id, GraphNodeKind::EvidenceUnit, "ev-1");
        let image_node = sample_graph_node(&source_id, GraphNodeKind::ImageArtifact, "img-1");
        let contains = sample_graph_edge(
            &source_id,
            EdgeType::Contains,
            &source_node.id,
            &chunk_node.id,
            Some(0),
        );
        let derives = sample_graph_edge(
            &source_id,
            EdgeType::DerivedFrom,
            &evidence_node.id,
            &image_node.id,
            None,
        );

        let nodes = vec![source_node, chunk_node.clone(), evidence_node, image_node];
        let edges = vec![contains.clone(), derives.clone()];
        store.upsert_graph_nodes(&nodes).unwrap();
        store.upsert_graph_edges(&edges).unwrap();
        store.upsert_graph_nodes(&nodes).unwrap();
        store.upsert_graph_edges(&edges).unwrap();

        assert_eq!(
            store.list_graph_nodes_by_source(&source_id).unwrap().len(),
            4
        );
        assert_eq!(
            store.list_graph_edges_by_source(&source_id).unwrap().len(),
            2
        );
        assert_eq!(
            store.list_graph_edges_from(&contains.from_node_id).unwrap(),
            vec![contains.clone()]
        );
        assert_eq!(
            store.list_graph_edges_to(&chunk_node.id).unwrap(),
            vec![contains]
        );
        assert_eq!(
            store
                .list_graph_edges_by_type(&source_id, EdgeType::DerivedFrom)
                .unwrap(),
            vec![derives]
        );

        store.remove_graph_by_source(&source_id).unwrap();
        assert!(store
            .list_graph_nodes_by_source(&source_id)
            .unwrap()
            .is_empty());
        assert!(store
            .list_graph_edges_by_source(&source_id)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn evidence_and_chunk_insert() {
        let store = Store::in_memory().unwrap();
        store.add_source(&sample_source()).unwrap();

        let evidence = sample_evidence("src-1");
        store.bulk_insert_evidence(&evidence).unwrap();

        let ev_list = store
            .list_evidence_by_source(&SourceId("src-1".into()))
            .unwrap();
        assert_eq!(ev_list.len(), 2);
        assert_eq!(ev_list[0].kind, EvidenceKind::Text);

        let chunks = sample_chunks("src-1");
        store.bulk_insert_chunks(&chunks).unwrap();

        store
            .link_chunk_evidence(&[
                (ChunkId("child-1".into()), EvidenceId("ev-1".into())),
                (ChunkId("parent-1".into()), EvidenceId("ev-1".into())),
                (ChunkId("parent-1".into()), EvidenceId("ev-2".into())),
            ])
            .unwrap();

        let child = store
            .get_chunk(&ChunkId("child-1".into()))
            .unwrap()
            .unwrap();
        assert_eq!(child.chunk_type, ChunkType::Child);
        assert_eq!(child.evidence_unit_ids.len(), 1);

        let parent = store
            .get_parent_chunk(&ChunkId("child-1".into()))
            .unwrap()
            .unwrap();
        assert_eq!(parent.id.0, "parent-1");
        assert_eq!(parent.evidence_unit_ids.len(), 2);
    }

    #[test]
    fn cascade_delete() {
        let store = Store::in_memory().unwrap();
        store.add_source(&sample_source()).unwrap();
        store
            .bulk_insert_evidence(&sample_evidence("src-1"))
            .unwrap();
        store.bulk_insert_chunks(&sample_chunks("src-1")).unwrap();
        store
            .link_chunk_evidence(&[(ChunkId("child-1".into()), EvidenceId("ev-1".into()))])
            .unwrap();
        store
            .set_embedding_meta(&ChunkId("child-1".into()), 0, "model", "2025-01-01")
            .unwrap();
        store
            .replace_all_vector_documents(&[VectorDocument {
                chunk_id: ChunkId("child-1".into()),
                source_id: SourceId("src-1".into()),
                vector: vec![1.0, 0.0],
            }])
            .unwrap();
        store
            .bulk_insert_image_artifacts(&[sample_image_artifact("src-1", "ev-1")])
            .unwrap();
        let source_id = SourceId("src-1".into());
        let source_node = sample_graph_node(&source_id, GraphNodeKind::Source, "src-1");
        let evidence_node = sample_graph_node(&source_id, GraphNodeKind::EvidenceUnit, "ev-1");
        let edge = sample_graph_edge(
            &source_id,
            EdgeType::Contains,
            &source_node.id,
            &evidence_node.id,
            Some(0),
        );
        store
            .upsert_graph_nodes(&[source_node, evidence_node])
            .unwrap();
        store.upsert_graph_edges(&[edge]).unwrap();
        assert_eq!(store.list_vector_documents().unwrap().len(), 1);
        assert_eq!(
            store
                .list_image_artifacts_by_source(&SourceId("src-1".into()))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            store.list_graph_nodes_by_source(&source_id).unwrap().len(),
            2
        );
        assert_eq!(
            store.list_graph_edges_by_source(&source_id).unwrap().len(),
            1
        );

        store.remove_source(&source_id).unwrap();

        assert!(store
            .get_evidence(&EvidenceId("ev-1".into()))
            .unwrap()
            .is_none());
        assert!(store
            .get_chunk(&ChunkId("child-1".into()))
            .unwrap()
            .is_none());
        assert!(store
            .get_embedding_meta(&ChunkId("child-1".into()))
            .unwrap()
            .is_none());
        assert!(store.list_vector_documents().unwrap().is_empty());
        assert!(store
            .list_image_artifacts_by_source(&SourceId("src-1".into()))
            .unwrap()
            .is_empty());
        assert!(store
            .list_graph_nodes_by_source(&source_id)
            .unwrap()
            .is_empty());
        assert!(store
            .list_graph_edges_by_source(&source_id)
            .unwrap()
            .is_empty());
    }

    #[test]
    fn image_artifact_lookup_by_evidence() {
        let store = Store::in_memory().unwrap();
        store.add_source(&sample_source()).unwrap();
        store
            .bulk_insert_evidence(&sample_evidence("src-1"))
            .unwrap();

        store
            .bulk_insert_image_artifacts(&[sample_image_artifact("src-1", "ev-1")])
            .unwrap();

        let artifact = store
            .get_image_artifact_by_evidence(&EvidenceId("ev-1".into()))
            .unwrap()
            .unwrap();

        assert_eq!(artifact.image_index, 1);
        assert_eq!(artifact.mime_type, "image/png");
        assert_eq!(
            artifact.bbox,
            Some(BBox {
                x0: 1.0,
                y0: 2.0,
                x1: 3.0,
                y1: 4.0,
            })
        );
    }

    #[test]
    fn list_child_chunks_excludes_parents() {
        let store = Store::in_memory().unwrap();
        store.add_source(&sample_source()).unwrap();
        store
            .bulk_insert_evidence(&sample_evidence("src-1"))
            .unwrap();
        store.bulk_insert_chunks(&sample_chunks("src-1")).unwrap();

        let children = store.list_child_chunks().unwrap();

        assert_eq!(children.len(), 1);
        assert_eq!(children[0].id.0, "child-1");
    }

    #[test]
    fn stale_detection() {
        let store = Store::in_memory().unwrap();
        store.add_source(&sample_source()).unwrap();

        let mut current = HashMap::new();
        current.insert(SourceId("src-1".into()), "different_hash".into());

        let stale = store.find_stale_sources(&current).unwrap();
        assert_eq!(stale.len(), 1);
        assert_eq!(stale[0].0, "src-1");

        current.insert(SourceId("src-1".into()), "abc123".into());
        let stale = store.find_stale_sources(&current).unwrap();
        assert!(stale.is_empty());
    }

    #[test]
    fn profile_stale_detection_includes_vector_state() {
        let store = Store::in_memory().unwrap();
        let mut source = sample_source();
        source.status = SourceStatus::Indexed;
        store.add_source(&source).unwrap();
        store
            .bulk_insert_evidence(&sample_evidence("src-1"))
            .unwrap();
        store.bulk_insert_chunks(&sample_chunks("src-1")).unwrap();

        let profile = EmbeddingProfileId::default_profile();
        let mut current = HashMap::new();
        current.insert(SourceId("src-1".into()), "abc123".into());

        let stale = store
            .find_stale_sources_for_profile(&current, &profile)
            .unwrap();
        assert_eq!(stale, vec![SourceId("src-1".into())]);

        store
            .replace_source_vector_documents_for_profile(
                &profile,
                &SourceId("src-1".into()),
                &[VectorDocument {
                    chunk_id: ChunkId("child-1".into()),
                    source_id: SourceId("src-1".into()),
                    vector: vec![1.0, 0.0],
                }],
            )
            .unwrap();

        let stale = store
            .find_stale_sources_for_profile(&current, &profile)
            .unwrap();
        assert!(stale.is_empty());

        store
            .set_source_embedding_status(
                &profile,
                &SourceId("src-1".into()),
                SourceEmbeddingStatus::Failed,
                1,
                Some("provider failed"),
            )
            .unwrap();

        let stale = store
            .find_stale_sources_for_profile(&current, &profile)
            .unwrap();
        assert_eq!(stale, vec![SourceId("src-1".into())]);
    }
}
