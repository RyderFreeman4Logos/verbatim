use std::collections::{HashMap, HashSet};

use anyhow::{bail, Context, Result};
use rusqlite::{params, params_from_iter, OptionalExtension};

use super::{row_to_evidence_unit, EmbeddingProfileId, EvidenceId, EvidenceUnit, Store};
use crate::types::report_artifact::is_report_artifact_id;

const SQLITE_VARIABLE_LIMIT: usize = 32_766;

type EvidenceBatchWithIndexGeneration = (HashMap<EvidenceId, Result<EvidenceUnit>>, Option<String>);

impl Store {
    pub(crate) fn get_evidence_with_index_generation(
        &self,
        id: &EvidenceId,
        profile_id: &EmbeddingProfileId,
    ) -> Result<Option<(EvidenceUnit, u64)>> {
        if is_report_artifact_id(&id.0) {
            bail!("report artifact ids are not evidence: {}", id.0);
        }
        let Some((evidence, generation)) = self
            .conn
            .query_row(
                "SELECT id, source_id, kind, locator_json, text, text_hash, heading_path_json, language, position, derived_from_evidence_id,
                        COALESCE((SELECT generation FROM embedding_profile_index_meta WHERE profile_id = ?2), '0')
                 FROM evidence_units WHERE id = ?1",
                params![id.0, profile_id.as_str()],
                |row| {
                    let evidence = row_to_evidence_unit(row)?;
                    let generation: String = row.get(10)?;
                    Ok((evidence, generation))
                },
            )
            .optional()?
        else {
            return Ok(None);
        };
        Ok(Some((
            evidence,
            generation
                .parse()
                .context("parse profile index generation")?,
        )))
    }

    /// Same batch evidence load, plus the executed profile index generation from that store read.
    pub fn get_evidence_batch_with_index_generation(
        &self,
        ids: &[EvidenceId],
        profile_id: &EmbeddingProfileId,
    ) -> Result<EvidenceBatchWithIndexGeneration> {
        if let Some(id) = ids.iter().find(|id| is_report_artifact_id(&id.0)) {
            bail!("report artifact ids are not evidence: {}", id.0);
        }
        let mut seen = HashSet::with_capacity(ids.len());
        let ids = ids
            .iter()
            .filter(|id| seen.insert(&id.0))
            .collect::<Vec<_>>();
        let mut evidence = HashMap::with_capacity(ids.len());
        let mut generation = None;
        for batch in ids.chunks(SQLITE_VARIABLE_LIMIT.saturating_sub(1).max(1)) {
            let placeholders = vec!["?"; batch.len()].join(", ");
            let sql = format!(
                "SELECT id, source_id, kind, locator_json, text, text_hash, heading_path_json, language, position, derived_from_evidence_id,
                        COALESCE((SELECT generation FROM embedding_profile_index_meta WHERE profile_id = ?), '0')
                 FROM evidence_units WHERE id IN ({placeholders})"
            );
            let mut params = Vec::with_capacity(batch.len() + 1);
            params.push(profile_id.as_str());
            params.extend(batch.iter().map(|id| id.0.as_str()));
            let mut stmt = self.conn.prepare(&sql)?;
            let mut rows = stmt.query(params_from_iter(params))?;
            while let Some(row) = rows.next()? {
                let id = EvidenceId(row.get(0)?);
                evidence.insert(id, row_to_evidence_unit(row).map_err(Into::into));
                if generation.is_none() {
                    let raw: String = row.get(10)?;
                    raw.parse::<u64>()
                        .context("parse profile index generation")?;
                    generation = Some(raw);
                }
            }
        }
        Ok((evidence, generation))
    }
}
