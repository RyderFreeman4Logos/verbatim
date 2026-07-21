use std::collections::HashMap;

use anyhow::{Context, Result};
use rusqlite::{params, Transaction};

use super::{from_json_error, Store};
use crate::evidence_spans::ChunkEvidenceSpan;
use crate::types::{ChunkId, EvidenceId};

impl Store {
    /// Return the persisted source-backed spans for one chunk in deterministic order.
    pub fn list_chunk_evidence_spans(&self, chunk_id: &ChunkId) -> Result<Vec<ChunkEvidenceSpan>> {
        let mut stmt = self.conn.prepare(
            "SELECT evidence_unit_id, chunk_byte_start, chunk_byte_end, evidence_byte_start,
                    evidence_byte_end, evidence_text_hash, locator_json, trust_json
             FROM chunk_evidence_spans WHERE chunk_id = ?1 ORDER BY ordinal",
        )?;
        let rows = stmt.query_map(params![&chunk_id.0], |row| {
            let locator_json: String = row.get(6)?;
            let trust_json: String = row.get(7)?;
            Ok(ChunkEvidenceSpan {
                chunk_id: chunk_id.clone(),
                evidence_id: EvidenceId(row.get(0)?),
                chunk_byte_start: row.get(1)?,
                chunk_byte_end: row.get(2)?,
                evidence_byte_start: row.get(3)?,
                evidence_byte_end: row.get(4)?,
                evidence_text_hash: row.get(5)?,
                locator: serde_json::from_str(&locator_json)
                    .map_err(|err| from_json_error(6, "SourceLocator", err))?,
                trust: serde_json::from_str(&trust_json)
                    .map_err(|err| from_json_error(7, "EvidenceSpanTrust", err))?,
            })
        })?;
        rows.map(|row| Ok(row?)).collect()
    }
}

pub(super) fn insert_chunk_evidence_spans_tx(
    tx: &Transaction<'_>,
    spans: &[ChunkEvidenceSpan],
) -> Result<()> {
    let mut statement = tx.prepare(
        "INSERT INTO chunk_evidence_spans
            (chunk_id, ordinal, evidence_unit_id, chunk_byte_start, chunk_byte_end,
             evidence_byte_start, evidence_byte_end, evidence_text_hash, locator_json, trust_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    let mut ordinals = HashMap::<&ChunkId, i64>::new();
    for span in spans {
        let ordinal = ordinals.entry(&span.chunk_id).or_insert(0);
        let locator_json =
            serde_json::to_string(&span.locator).context("serialize span locator")?;
        let trust_json = serde_json::to_string(&span.trust).context("serialize span trust")?;
        statement.execute(params![
            &span.chunk_id.0,
            *ordinal,
            &span.evidence_id.0,
            i64::try_from(span.chunk_byte_start)
                .context("chunk span start exceeds SQLite range")?,
            i64::try_from(span.chunk_byte_end).context("chunk span end exceeds SQLite range")?,
            i64::try_from(span.evidence_byte_start)
                .context("evidence span start exceeds SQLite range")?,
            i64::try_from(span.evidence_byte_end)
                .context("evidence span end exceeds SQLite range")?,
            &span.evidence_text_hash,
            locator_json,
            trust_json,
        ])?;
        *ordinal = ordinal.saturating_add(1);
    }
    Ok(())
}
