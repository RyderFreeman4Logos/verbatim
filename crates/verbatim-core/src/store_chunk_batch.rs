use std::collections::{hash_map::Entry, HashMap, HashSet};

use anyhow::Result;
use rusqlite::params_from_iter;

use super::{
    row_to_chunk_tuple, tuple_to_chunk_with_evidence_ids, Chunk, ChunkId, EvidenceId, Store,
};

const SQLITE_VARIABLE_LIMIT: usize = 32_766;

impl Store {
    /// Load a bounded set of chunks and their evidence links in batches.
    ///
    /// Missing IDs are omitted. Each returned entry keeps candidate-local
    /// conversion or evidence-link errors separate from batch query errors.
    pub fn get_chunks(&self, ids: &[ChunkId]) -> Result<HashMap<ChunkId, Result<Chunk>>> {
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
            let mut rows = stmt.query(params_from_iter(batch.iter().map(|id| &id.0)))?;
            while let Some(row) = rows.next()? {
                let id = ChunkId(row.get(0)?);
                chunks.insert(id, row_to_chunk_tuple(row).map_err(Into::into));
            }
        }

        let chunk_ids = chunks.keys().map(|id| id.0.as_str()).collect::<Vec<_>>();
        let mut evidence_ids = get_evidence_ids_for_chunks(&self.conn, &chunk_ids)?;
        Ok(chunks
            .into_iter()
            .map(|(id, chunk)| {
                let chunk = chunk.and_then(|chunk| {
                    let evidence = match evidence_ids.remove(&id) {
                        Some(Ok(evidence)) => evidence,
                        Some(Err(error)) => return Err(error),
                        None => Vec::new(),
                    };
                    tuple_to_chunk_with_evidence_ids(chunk, evidence)
                });
                (id, chunk)
            })
            .collect())
    }
}

fn get_evidence_ids_for_chunks(
    conn: &rusqlite::Connection,
    chunk_ids: &[&str],
) -> Result<HashMap<ChunkId, Result<Vec<EvidenceId>>>> {
    let mut evidence_ids = HashMap::new();
    for batch in chunk_ids.chunks(SQLITE_VARIABLE_LIMIT) {
        let placeholders = vec!["?"; batch.len()].join(", ");
        let sql = format!(
            "SELECT chunk_id, evidence_unit_id FROM chunk_evidence WHERE chunk_id IN ({placeholders}) ORDER BY rowid"
        );
        let mut stmt = conn.prepare(&sql)?;
        let mut rows = stmt.query(params_from_iter(batch.iter()))?;
        while let Some(row) = rows.next()? {
            let chunk_id = ChunkId(row.get(0)?);
            match row.get(1) {
                Ok(evidence_id) => match evidence_ids.entry(chunk_id) {
                    Entry::Vacant(entry) => {
                        entry.insert(Ok(vec![EvidenceId(evidence_id)]));
                    }
                    Entry::Occupied(mut entry) => {
                        if let Ok(evidence_ids) = entry.get_mut() {
                            evidence_ids.push(EvidenceId(evidence_id));
                        }
                    }
                },
                Err(error) => {
                    evidence_ids.insert(chunk_id, Err(error.into()));
                }
            }
        }
    }
    Ok(evidence_ids)
}
