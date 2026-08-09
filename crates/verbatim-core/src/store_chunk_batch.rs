use std::collections::{HashMap, HashSet};

use anyhow::Result;
use rusqlite::params_from_iter;

use super::{
    row_to_chunk_tuple, tuple_to_chunk_with_evidence_ids, Chunk, ChunkId, EvidenceId, Store,
};

const SQLITE_VARIABLE_LIMIT: usize = 32_766;

impl Store {
    /// Load a bounded set of chunks and their evidence links in batches.
    ///
    /// Missing IDs are omitted. Callers preserve their own rank order by looking
    /// up IDs in the returned map.
    pub fn get_chunks(&self, ids: &[ChunkId]) -> Result<HashMap<ChunkId, Chunk>> {
        let mut seen = HashSet::with_capacity(ids.len());
        let ids = ids
            .iter()
            .filter(|id| seen.insert(&id.0))
            .collect::<Vec<_>>();
        if ids.is_empty() {
            return Ok(HashMap::new());
        }

        let mut chunks = HashMap::with_capacity(ids.len());
        for batch in ids.chunks(SQLITE_VARIABLE_LIMIT) {
            let placeholders = vec!["?"; batch.len()].join(", ");
            let sql = format!(
                "SELECT id, source_id, chunk_hash, embedding_input_hash, text, context_text, token_count, chunk_type, parent_chunk_id, heading_path_json FROM chunks WHERE id IN ({placeholders})"
            );
            let mut stmt = self.conn.prepare(&sql)?;
            let rows = stmt.query_map(
                params_from_iter(batch.iter().map(|id| &id.0)),
                row_to_chunk_tuple,
            )?;
            for row in rows {
                let chunk = row?;
                chunks.insert(chunk.0.clone(), chunk);
            }
        }

        let chunk_ids = chunks.keys().map(String::as_str).collect::<Vec<_>>();
        let mut evidence_ids = get_evidence_ids_for_chunks(&self.conn, &chunk_ids)?;
        chunks
            .into_iter()
            .map(|(id, chunk)| {
                let evidence = evidence_ids.remove(&id).unwrap_or_default();
                Ok((
                    ChunkId(id),
                    tuple_to_chunk_with_evidence_ids(chunk, evidence)?,
                ))
            })
            .collect()
    }
}

fn get_evidence_ids_for_chunks(
    conn: &rusqlite::Connection,
    chunk_ids: &[&str],
) -> Result<HashMap<String, Vec<EvidenceId>>> {
    let mut evidence_ids = HashMap::<String, Vec<EvidenceId>>::new();
    for batch in chunk_ids.chunks(SQLITE_VARIABLE_LIMIT) {
        let placeholders = vec!["?"; batch.len()].join(", ");
        let sql = format!(
            "SELECT chunk_id, evidence_unit_id FROM chunk_evidence WHERE chunk_id IN ({placeholders}) ORDER BY rowid"
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(params_from_iter(batch.iter()), |row| {
            Ok((row.get::<_, String>(0)?, EvidenceId(row.get(1)?)))
        })?;
        for row in rows {
            let (chunk_id, evidence_id) = row?;
            evidence_ids.entry(chunk_id).or_default().push(evidence_id);
        }
    }
    Ok(evidence_ids)
}
