use anyhow::{bail, Context, Result};
use rusqlite::{params, OptionalExtension};

use super::{row_to_evidence_unit, EmbeddingProfileId, EvidenceId, EvidenceUnit, Store};
use crate::types::report_artifact::is_report_artifact_id;

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
}
