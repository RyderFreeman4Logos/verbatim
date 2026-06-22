use std::collections::HashMap;
use std::path::{Path, PathBuf};

use anyhow::{Context, Result};
use rusqlite::{
    params, params_from_iter,
    types::{Type, Value},
    Connection, OptionalExtension, Transaction,
};

use crate::traits::VectorDocument;
use crate::types::{
    Chunk, ChunkId, ChunkType, EdgeType, EvidenceId, EvidenceKind, EvidenceUnit, GraphEdge,
    GraphEdgeId, GraphNode, GraphNodeId, GraphNodeKind, ImageArtifact, ImageId, Source, SourceId,
    SourceStatus,
};
use crate::vision_caption::{CaptionAttempt, ImageCaption, ImageCaptionRecord, ImageCaptionStatus};

pub struct Store {
    conn: Connection,
}

pub struct SourceContentsReplacement<'a> {
    pub source: &'a Source,
    pub evidence: &'a [EvidenceUnit],
    pub chunks: &'a [Chunk],
    pub vectors: &'a [VectorDocument],
    pub links: &'a [(ChunkId, EvidenceId)],
    pub image_artifacts: &'a [ImageArtifact],
    pub graph_nodes: &'a [GraphNode],
    pub graph_edges: &'a [GraphEdge],
}

impl Store {
    pub fn new(path: &Path) -> Result<Self> {
        let conn = Connection::open(path)?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    pub fn in_memory() -> Result<Self> {
        let conn = Connection::open_in_memory()?;
        let store = Self { conn };
        store.migrate()?;
        Ok(store)
    }

    fn migrate(&self) -> Result<()> {
        self.conn.execute_batch("PRAGMA foreign_keys = ON;")?;
        self.conn.execute_batch(SCHEMA)?;
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
        let generation = bump_index_generation(&tx)?;
        tx.commit()?;
        Ok(generation)
    }

    pub fn remove_source_and_replace_vectors(
        &self,
        id: &SourceId,
        vectors: &[VectorDocument],
    ) -> Result<u64> {
        let tx = self.conn.unchecked_transaction()?;
        tx.execute("DELETE FROM sources WHERE id = ?1", params![&id.0])?;
        replace_vector_documents_tx(&tx, vectors)?;
        let generation = bump_index_generation(&tx)?;
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
        replace_vector_documents_tx(&tx, vectors)?;

        let generation = bump_index_generation(&tx)?;
        tx.commit()?;
        Ok(generation)
    }

    pub fn index_generation(&self) -> Result<u64> {
        let value: Option<String> = self
            .conn
            .query_row(
                "SELECT value FROM index_meta WHERE key = 'generation'",
                [],
                |row| row.get(0),
            )
            .optional()?;
        value
            .as_deref()
            .unwrap_or("0")
            .parse::<u64>()
            .context("parse index generation")
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

    pub fn set_embedding_meta(
        &self,
        chunk_id: &ChunkId,
        hnsw_position: i64,
        embedding_model: &str,
        embedded_at: &str,
    ) -> Result<()> {
        self.conn.execute(
            "INSERT OR REPLACE INTO embeddings_meta (chunk_id, hnsw_position, embedding_model, embedded_at) VALUES (?1, ?2, ?3, ?4)",
            params![chunk_id.0, hnsw_position, embedding_model, embedded_at],
        )?;
        Ok(())
    }

    pub fn get_embedding_meta(&self, chunk_id: &ChunkId) -> Result<Option<(i64, String, String)>> {
        let mut stmt = self.conn.prepare(
            "SELECT hnsw_position, embedding_model, embedded_at FROM embeddings_meta WHERE chunk_id = ?1"
        )?;
        let mut rows = stmt.query_map(params![chunk_id.0], |row| {
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
        let tx = self.conn.unchecked_transaction()?;
        replace_vector_documents_tx(&tx, vectors)?;
        tx.commit()?;
        Ok(())
    }

    pub fn list_vector_documents(&self) -> Result<Vec<VectorDocument>> {
        let mut stmt = self.conn.prepare(
            "SELECT chunk_id, source_id, vector_json FROM chunk_vectors ORDER BY source_id, chunk_id",
        )?;
        let rows = stmt.query_map([], |row| {
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

fn replace_vector_documents_tx(tx: &Transaction<'_>, vectors: &[VectorDocument]) -> Result<()> {
    tx.execute("DELETE FROM chunk_vectors", [])
        .context("clear chunk vectors")?;
    let mut stmt = tx
        .prepare("INSERT INTO chunk_vectors (chunk_id, source_id, vector_json) VALUES (?1, ?2, ?3)")
        .context("prepare vector insert")?;
    for vector in vectors {
        let vector_json = serde_json::to_string(&vector.vector).context("serialize vector")?;
        stmt.execute(params![
            &vector.chunk_id.0,
            &vector.source_id.0,
            vector_json,
        ])
        .with_context(|| format!("insert vector for chunk {}", vector.chunk_id.0))?;
    }
    Ok(())
}

fn bump_index_generation(tx: &Transaction<'_>) -> Result<u64> {
    let current: Option<String> = tx
        .query_row(
            "SELECT value FROM index_meta WHERE key = 'generation'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    let next = current
        .as_deref()
        .unwrap_or("0")
        .parse::<u64>()
        .context("parse current index generation")?
        .saturating_add(1);
    tx.execute(
        "INSERT OR REPLACE INTO index_meta (key, value) VALUES ('generation', ?1)",
        params![next.to_string()],
    )?;
    Ok(next)
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
        EvidenceKind::Image => "Image",
        EvidenceKind::Generated => "Generated",
    }
}

fn str_to_evidence_kind(kind: &str) -> EvidenceKind {
    match kind {
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
CREATE TABLE IF NOT EXISTS chunk_vectors (
    chunk_id TEXT PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
    source_id TEXT NOT NULL REFERENCES sources(id) ON DELETE CASCADE,
    vector_json TEXT NOT NULL
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
CREATE TABLE IF NOT EXISTS embeddings_meta (
    chunk_id TEXT PRIMARY KEY REFERENCES chunks(id) ON DELETE CASCADE,
    hnsw_position INTEGER NOT NULL,
    embedding_model TEXT NOT NULL,
    embedded_at TEXT NOT NULL
);
CREATE TABLE IF NOT EXISTS index_meta (
    key TEXT PRIMARY KEY,
    value TEXT NOT NULL
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
    use crate::types::{BBox, SourceLocator};
    use std::path::PathBuf;

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
}
