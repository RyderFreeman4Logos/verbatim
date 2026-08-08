use anyhow::{ensure, Context, Result};

use crate::store::Store;
use crate::types::{hex_sha256, EvidenceId, EvidenceUnit};

impl Store {
    /// Reload evidence from durable storage and verify its canonical text hash.
    pub fn resolve_source_bounded_evidence(&self, id: &EvidenceId) -> Result<EvidenceUnit> {
        let evidence = self
            .get_evidence(id)?
            .with_context(|| format!("source-bounded evidence not found: {}", id.0))?;
        let actual_text_hash = hex_sha256(evidence.text.as_bytes());
        ensure!(
            evidence.text_hash == actual_text_hash,
            "source-bounded evidence text hash mismatch: {}",
            id.0
        );
        Ok(evidence)
    }
}
