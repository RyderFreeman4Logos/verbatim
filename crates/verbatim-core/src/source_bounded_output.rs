use anyhow::{ensure, Context, Result};

use crate::store::Store;
use crate::types::{hex_sha256, EmbeddingProfileId, EvidenceUnit};

impl Store {
    /// Reload retrieval-time evidence and reject changed durable records.
    pub fn resolve_source_bounded_evidence(&self, expected: &EvidenceUnit) -> Result<EvidenceUnit> {
        let evidence = self
            .get_evidence(&expected.id)?
            .with_context(|| format!("source-bounded evidence not found: {}", expected.id.0))?;
        ensure_unchanged_source_bounded_evidence(expected, evidence)
    }

    /// Same reload, plus the executed profile index generation from that store read.
    pub fn resolve_source_bounded_evidence_for_profile(
        &self,
        expected: &EvidenceUnit,
        profile_id: &EmbeddingProfileId,
    ) -> Result<(EvidenceUnit, u64)> {
        let (evidence, generation) = self
            .get_evidence_with_index_generation(&expected.id, profile_id)?
            .with_context(|| format!("source-bounded evidence not found: {}", expected.id.0))?;
        Ok((
            ensure_unchanged_source_bounded_evidence(expected, evidence)?,
            generation,
        ))
    }
}

fn ensure_unchanged_source_bounded_evidence(
    expected: &EvidenceUnit,
    evidence: EvidenceUnit,
) -> Result<EvidenceUnit> {
    let actual_text_hash = hex_sha256(evidence.text.as_bytes());
    ensure!(
        evidence.text_hash == actual_text_hash,
        "source-bounded evidence text hash mismatch: {}",
        expected.id.0
    );
    ensure!(
        evidence.text_hash == expected.text_hash,
        "source-bounded evidence text changed since retrieval: {}",
        expected.id.0
    );
    ensure!(
        evidence.source_id == expected.source_id
            && evidence.kind == expected.kind
            && evidence.derived_from == expected.derived_from
            && evidence.locator == expected.locator
            && evidence.heading_path == expected.heading_path
            && evidence.position == expected.position,
        "source-bounded evidence identity changed since retrieval: {}",
        expected.id.0
    );
    Ok(evidence)
}
