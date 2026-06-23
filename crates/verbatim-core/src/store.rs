use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{bail, Context, Result};
use rusqlite::{
    params, params_from_iter,
    types::{Type, Value},
    Connection, OptionalExtension, Transaction,
};

use crate::task::{
    bounded_error, bounded_json, bounded_message, TaskEvent, TaskId, TaskKind, TaskSpan,
    TaskStatus, TaskSummary,
};
use crate::traits::VectorDocument;
use crate::types::{
    hex_sha256, Chunk, ChunkId, ChunkType, EdgeType, EmbeddingProfileId, EvidenceId, EvidenceKind,
    EvidenceUnit, GraphEdge, GraphEdgeId, GraphNode, GraphNodeId, GraphNodeKind, ImageArtifact,
    ImageId, Source, SourceEmbeddingStatus, SourceId, SourceStatus, DEFAULT_EMBEDDING_PROFILE_ID,
};
use crate::vision_caption::{CaptionAttempt, ImageCaption, ImageCaptionRecord, ImageCaptionStatus};

const LEGACY_EMBEDDING_PROFILE_CONFIG_HASH: &str = "legacy";
const SQLITE_BUSY_TIMEOUT: Duration = Duration::from_secs(30);

pub struct Store {
    conn: Connection,
}

pub struct SourceContentsReplacement<'a> {
    pub source: &'a Source,
    pub evidence: &'a [EvidenceUnit],
    pub chunks: &'a [Chunk],
    pub embedding_profile_id: &'a EmbeddingProfileId,
    pub vectors: &'a [VectorDocument],
    pub links: &'a [(ChunkId, EvidenceId)],
    pub image_artifacts: &'a [ImageArtifact],
    pub graph_nodes: &'a [GraphNode],
    pub graph_edges: &'a [GraphEdge],
}

/// Stable configuration that defines an embedding profile's vector semantics.
#[derive(Clone, Copy, Debug)]
pub struct EmbeddingProfileConfig<'a> {
    pub provider: &'a str,
    pub model: &'a str,
    pub dimension: usize,
    pub normalize: bool,
    pub query_instruction: &'a str,
    pub document_instruction: &'a str,
}

impl Store {
    pub fn new(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        conn.busy_timeout(SQLITE_BUSY_TIMEOUT)?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        self.conn.execute_batch(SCHEMA)?;
        migrate_embedding_profile_tables(&self.conn)?;
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
        Ok(())
    }

    pub(crate) fn connection(&self) -> &Connection {
        &self.conn
    }

    // --- Source ---

    pub fn add_source(&self, source: &Source) -> Result<()> {
        self.conn.execute(
            "INSERT INTO sources (id, path, hash, status, parser_used, last_ingested_at) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                source.id.0,
                source.path.to_str().unwrap_or(""),
                source.hash,
                status_to_str(&source.status),
                source.parser_used,
                source.last_ingested_at,
            ],
        )?;
        Ok(())
    }

    pub fn list_sources(&self) -> Result<Vec<Source>> {
        let mut stmt = self
            .conn
            .prepare("SELECT id, path, hash, status, parser_used, last_ingested_at FROM sources")?;
        let rows = stmt.query_map([], |row| {
            Ok(Source {
                id: SourceId(row.get(0)?),
                path: std::path::PathBuf::from(row.get::<_, String>(1)?),
                hash: row.get(2)?,
                status: str_to_status(&row.get::<_, String>(3)?),
                parser_used: row.get(4)?,
                last_ingested_at: row.get(5)?,
            })
        })?;
        rows.map(|r| r.map_err(Into::into)).collect()
    }

    pub fn get_source(&self, id: &SourceId) -> Result<Option<Source>> {
        let mut stmt = self.conn.prepare("SELECT id, path, hash, status, parser_used, last_ingested_at FROM sources WHERE id = ?1")?;
        let mut rows = stmt.query_map(params![id.0], |row| {
            Ok(Source {
                id: SourceId(row.get(0)?),
                path: std::path::PathBuf::from(row.get::<_, String>(1)?),
                hash: row.get(2)?,
                status: str_to_status(&row.get::<_, String>(3)?),
                parser_used: row.get(4)?,
                last_ingested_at: row.get(5)?,
            })
        })?;
        match rows.next() {
            Some(r) => Ok(Some(r?)),
            None => Ok(None),
        }
    }

    pub fn remove_source(&self, id: &SourceId) -> Result<u64> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM sources WHERE id = ?1", params![&id.0])?;
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
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM sources WHERE id = ?1", params![&id.0])?;
        replace_vector_documents_for_profile_tx(&tx, profile_id, vectors)?;
        let generation = bump_all_profile_index_generations(&tx)?;
        tx.commit()?;
        Ok(generation)
    }

    pub fn replace_source_contents(
        &self,
        replacement: SourceContentsReplacement<'_>,
    ) -> Result<u64> {
        let SourceContentsReplacement {
            source,
            evidence,
            chunks,
            embedding_profile_id,
            vectors,
            links,
            image_artifacts,
            graph_nodes,
            graph_edges,
        } = replacement;
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM sources WHERE id = ?1", params![&source.id.0])?;
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

        insert_evidence_units_tx(&tx, evidence)?;

        {
            let mut stmt = tx.prepare(
                "INSERT INTO chunks (id, source_id, text, context_text, token_count, chunk_type, parent_chunk_id, heading_path_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
            )?;
            for chunk in chunks {
                let heading_json =
                    serde_json::to_string(&chunk.heading_path).context("serialize heading_path")?;
                stmt.execute(params![
                    &chunk.id.0,
                    &chunk.source_id.0,
                    &chunk.text,
                    &chunk.context_text,
                    chunk.token_count,
                    chunk_type_to_str(&chunk.chunk_type),
                    chunk.parent_chunk_id.as_ref().map(|id| &id.0),
                    heading_json,
                ])?;
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

        insert_image_artifacts_tx(&tx, image_artifacts)?;
        upsert_graph_nodes_tx(&tx, graph_nodes)?;
        upsert_graph_edges_tx(&tx, graph_edges)?;
        replace_source_vector_documents_for_profile_tx(
            &tx,
            embedding_profile_id,
            &source.id,
            vectors,
        )?;
        set_source_embedding_status_tx(
            &tx,
            embedding_profile_id,
            &source.id,
            SourceEmbeddingStatus::Embedded,
            vectors.len(),
            None,
        )?;

        let generation = bump_all_profile_index_generations(&tx)?;
        tx.commit()?;
        Ok(generation)
    }

    pub fn index_generation(&self) -> Result<u64> {
        self.index_generation_for_profile(&EmbeddingProfileId::default_profile())
    }

    pub fn index_generation_for_profile(&self, profile_id: &EmbeddingProfileId) -> Result<u64> {
        let value: Option<String> = self
            .conn
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

    fn source_vectors_stale_for_profile(
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
                "INSERT INTO chunks (id, source_id, text, context_text, token_count, chunk_type, parent_chunk_id, heading_path_json) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)"
            )?;
            for c in chunks {
                let heading_json =
                    serde_json::to_string(&c.heading_path).context("serialize heading_path")?;
                stmt.execute(params![
                    c.id.0,
                    c.source_id.0,
                    c.text,
                    c.context_text,
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
            "SELECT id, source_id, text, context_text, token_count, chunk_type, parent_chunk_id, heading_path_json FROM chunks WHERE id = ?1"
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
            "SELECT id, source_id, text, context_text, token_count, chunk_type, parent_chunk_id, heading_path_json FROM chunks WHERE source_id = ?1"
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
        "SELECT id, source_id, text, context_text, token_count, chunk_type, parent_chunk_id, heading_path_json FROM chunks WHERE chunk_type = 'Child' ORDER BY source_id, id"
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
            "SELECT c.id, c.source_id, c.text, c.context_text, c.token_count, c.chunk_type, c.parent_chunk_id, c.heading_path_json
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
    ) -> Result<()> {
        let now = unix_timestamp_string();
        let query_instruction_hash = embedding_instruction_hash(config.query_instruction);
        let document_instruction_hash = embedding_instruction_hash(config.document_instruction);
        let config_hash = embedding_profile_config_hash(
            config.provider,
            config.model,
            config.dimension,
            config.normalize,
            &query_instruction_hash,
            &document_instruction_hash,
        );
        self.conn.execute(
            "INSERT INTO embedding_profiles
                (id, provider, model, dimension, normalize, query_instruction_hash, document_instruction_hash, config_hash, qdrant_collection, qdrant_vector_name, created_at, updated_at)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, NULL, NULL, ?9, ?9)
             ON CONFLICT(id) DO NOTHING",
            params![
                profile_id.as_str(),
                config.provider,
                config.model,
                sql_usize(config.dimension),
                config.normalize,
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

        let existing: Option<(String, String, i64, bool, String, String, String)> = self
            .conn
            .query_row(
                "SELECT provider, model, dimension, normalize, query_instruction_hash, document_instruction_hash, config_hash
                 FROM embedding_profiles
                 WHERE id = ?1",
                params![profile_id.as_str()],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                        row.get(6)?,
                    ))
                },
            )
            .optional()?;
        let Some((
            existing_provider,
            existing_model,
            existing_dimension,
            existing_normalize,
            existing_query_instruction_hash,
            existing_document_instruction_hash,
            existing_hash,
        )) = existing
        else {
            bail!("embedding profile was not persisted: {profile_id}");
        };
        if existing_hash == LEGACY_EMBEDDING_PROFILE_CONFIG_HASH {
            let now = unix_timestamp_string();
            self.conn.execute(
                "UPDATE embedding_profiles
                 SET provider = ?2,
                     model = ?3,
                     dimension = ?4,
                     normalize = ?5,
                     query_instruction_hash = ?6,
                     document_instruction_hash = ?7,
                     config_hash = ?8,
                     updated_at = ?9
                 WHERE id = ?1",
                params![
                    profile_id.as_str(),
                    config.provider,
                    config.model,
                    sql_usize(config.dimension),
                    config.normalize,
                    query_instruction_hash,
                    document_instruction_hash,
                    config_hash,
                    now,
                ],
            )?;
            return Ok(());
        }
        if existing_hash != config_hash {
            bail!(
                "embedding profile '{}' already exists for provider='{}', model='{}', dimension={}, normalize={}, query_instruction_hash='{}', document_instruction_hash='{}', but current config is provider='{}', model='{}', dimension={}, normalize={}, query_instruction_hash='{}', document_instruction_hash='{}'",
                profile_id,
                existing_provider,
                existing_model,
                existing_dimension,
                existing_normalize,
                existing_query_instruction_hash,
                existing_document_instruction_hash,
                config.provider,
                config.model,
                config.dimension,
                config.normalize,
                query_instruction_hash,
                document_instruction_hash,
            );
        }

        Ok(())
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
            "SELECT chunk_id, source_id, vector_json
             FROM chunk_vectors
             WHERE profile_id = ?1
             ORDER BY source_id, chunk_id",
        )?;
        let rows = stmt.query_map(params![profile_id.as_str()], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })?;

        let mut result = Vec::new();
        for row in rows {
            let (chunk_id, source_id, vector_json) = row?;
            result.push(VectorDocument {
                chunk_id: ChunkId(chunk_id),
                source_id: SourceId(source_id),
                vector: serde_json::from_str(&vector_json).context("parse stored vector")?,
            });
        }
        Ok(result)
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
            "INSERT INTO tasks (id, kind, status, created_at, updated_at, started_at, finished_at, request_json, result_json, error)
             VALUES (?1, ?2, ?3, ?4, ?4, NULL, NULL, ?5, NULL, NULL)",
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
        })
    }

    pub fn get_task(&self, task_id: &TaskId) -> Result<Option<TaskSummary>> {
        self.conn
            .prepare(
                "SELECT id, kind, status, created_at, updated_at, started_at, finished_at, request_json, result_json, error
                 FROM tasks WHERE id = ?1",
            )?
            .query_row(params![&task_id.0], row_to_task_summary)
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
                "SELECT id, kind, status, created_at, updated_at, started_at, finished_at, request_json, result_json, error
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
            "SELECT id, kind, status, created_at, updated_at, started_at, finished_at, request_json, result_json, error
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
        let now = unix_timestamp_string();
        let result = serde_json::to_string(&bounded_json(result.clone()))
            .context("serialize task success result")?;
        let changed = self.conn.execute(
            "UPDATE tasks
             SET status = ?2, updated_at = ?3, finished_at = COALESCE(finished_at, ?3), result_json = ?4, error = NULL
             WHERE id = ?1 AND status != ?5",
            params![
                &task_id.0,
                TaskStatus::Succeeded.as_str(),
                now,
                result,
                TaskStatus::Cancelled.as_str(),
            ],
        )?;
        Ok(changed > 0)
    }

    pub fn finish_task_failed(&self, task_id: &TaskId, error: &str) -> Result<bool> {
        let now = unix_timestamp_string();
        let changed = self.conn.execute(
            "UPDATE tasks
             SET status = ?2, updated_at = ?3, finished_at = COALESCE(finished_at, ?3), error = ?4
             WHERE id = ?1 AND status != ?5",
            params![
                &task_id.0,
                TaskStatus::Failed.as_str(),
                now,
                bounded_error(error),
                TaskStatus::Cancelled.as_str(),
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
    let request = serde_json::from_str(&request_json)
        .map_err(|err| from_json_error(7, "serde_json::Value", err))?;
    let result = result_json
        .map(|json| {
            serde_json::from_str(&json).map_err(|err| from_json_error(8, "serde_json::Value", err))
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
    let empty_instruction_hash = embedding_instruction_hash("");
    for (id, provider, model, dimension, normalize, old_config_hash) in rows {
        if old_config_hash == LEGACY_EMBEDDING_PROFILE_CONFIG_HASH {
            continue;
        }
        let dimension = usize::try_from(dimension).with_context(|| {
            format!("invalid embedding profile dimension for profile '{id}': {dimension}")
        })?;
        let config_hash = embedding_profile_config_hash(
            &provider,
            &model,
            dimension,
            normalize,
            &empty_instruction_hash,
            &empty_instruction_hash,
        );
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

fn embedding_profile_config_hash(
    provider: &str,
    model: &str,
    dimension: usize,
    normalize: bool,
    query_instruction_hash: &str,
    document_instruction_hash: &str,
) -> String {
    hex_sha256(
        format!(
            "{provider}\0{model}\0{dimension}\0{normalize}\0{query_instruction_hash}\0{document_instruction_hash}"
        )
        .as_bytes(),
    )
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
    ))
}

fn tuple_to_chunk(t: ChunkTuple, conn: &Connection) -> Result<Chunk> {
    let (id, source_id, text, context_text, token_count, chunk_type, parent_id, heading_json) = t;
    let evidence_unit_ids = get_evidence_ids_for_chunk(conn, &id)?;
    Ok(Chunk {
        id: ChunkId(id),
        source_id: SourceId(source_id),
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
            "INSERT INTO chunk_vectors (profile_id, chunk_id, source_id, vector_json)
             VALUES (?1, ?2, ?3, ?4)",
        )
        .context("prepare vector insert")?;
    for vector in vectors {
        let vector_json = serde_json::to_string(&vector.vector).context("serialize vector")?;
        stmt.execute(params![
            profile_id.as_str(),
            &vector.chunk_id.0,
            &vector.source_id.0,
            vector_json,
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

const SCHEMA: &str = r#"
CREATE TABLE IF NOT EXISTS sources (
    id TEXT PRIMARY KEY,
    path TEXT NOT NULL,
    hash TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'Pending',
    parser_used TEXT,
    last_ingested_at TEXT
);
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
CREATE TABLE IF NOT EXISTS embedding_profiles (
    id TEXT PRIMARY KEY,
    provider TEXT NOT NULL,
    model TEXT NOT NULL,
    dimension INTEGER NOT NULL,
    normalize INTEGER NOT NULL,
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
    error TEXT
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
    use crate::task::{
        ask_request_metadata, ask_result_metadata, ingest_request_metadata, PhaseTiming,
        TASK_EVENT_MESSAGE_MAX_CHARS,
    };
    use crate::types::{BBox, SourceLocator};
    use std::path::PathBuf;
    use tempfile::tempdir;

    fn sample_source() -> Source {
        Source {
            id: SourceId("src-1".into()),
            path: PathBuf::from("/tmp/test.pdf"),
            hash: "abc123".into(),
            status: SourceStatus::Pending,
            parser_used: Some("pdf_oxide".into()),
            last_ingested_at: None,
        }
    }

    fn sample_evidence(source_id: &str) -> Vec<EvidenceUnit> {
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

    fn sample_chunks(source_id: &str) -> Vec<Chunk> {
        vec![
            Chunk {
                id: ChunkId("parent-1".into()),
                source_id: SourceId(source_id.into()),
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

    fn profile_id(id: &str) -> EmbeddingProfileId {
        EmbeddingProfileId::new(id).unwrap()
    }

    fn test_profile_config<'a>(
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
        let event = store
            .insert_task_event(
                &task_id,
                "phase",
                &"x".repeat(TASK_EVENT_MESSAGE_MAX_CHARS + 20),
                &serde_json::json!({"api_key": "should-not-print", "safe": "ok"}),
            )
            .unwrap();
        assert_eq!(event.sequence, 1);
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
        assert!(!encoded.contains("Do not persist this raw prompt"));
        assert!(!encoded.contains("Do not persist this raw answer"));
        assert!(!encoded.contains("should-not-print"));

        let events = store.list_task_events(&task_id, None, 100).unwrap();
        assert_eq!(events.len(), 1);
        assert!(store
            .list_task_events(&task_id, Some(events[0].sequence), 100)
            .unwrap()
            .is_empty());
        let spans = store.list_task_spans(&task_id).unwrap();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].metadata["model"], "qwen");
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
