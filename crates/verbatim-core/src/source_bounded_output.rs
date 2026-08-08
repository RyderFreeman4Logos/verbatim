use anyhow::{ensure, Context, Result};

use crate::store::Store;
use crate::types::{hex_sha256, EvidenceUnit};

impl Store {
    /// Reload retrieval-time evidence and reject changed durable records.
    pub fn resolve_source_bounded_evidence(&self, expected: &EvidenceUnit) -> Result<EvidenceUnit> {
        let evidence = self
            .get_evidence(&expected.id)?
            .with_context(|| format!("source-bounded evidence not found: {}", expected.id.0))?;
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
}
