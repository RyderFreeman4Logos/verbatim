//! Server-resolved evidence and bounded retrieval-candidate contracts.

use serde::{Deserialize, Serialize};

use super::util::{require_non_empty, require_sha256};
use super::{CitationAuditError, CitationAuditResult};

/// A source item resolved by the server before it can support an audit result.
/// The locator is retained here, rather than supplied by model output.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedEvidence {
    pub evidence_id: String,
    pub source_hash: String,
    pub locator: String,
    pub text: String,
}

impl ResolvedEvidence {
    pub fn validate(&self) -> CitationAuditResult<()> {
        require_non_empty("resolved_evidence.evidence_id", &self.evidence_id)?;
        require_sha256("resolved_evidence.source_hash", &self.source_hash)?;
        require_non_empty("resolved_evidence.locator", &self.locator)?;
        require_non_empty("resolved_evidence.text", &self.text)
    }
}

/// Registry of trusted, server-resolved evidence. Construction rejects duplicate
/// opaque IDs, so a reference has one deterministic source and locator.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceRegistry {
    evidence: Vec<ResolvedEvidence>,
}

impl EvidenceRegistry {
    pub fn new(evidence: Vec<ResolvedEvidence>) -> CitationAuditResult<Self> {
        let registry = Self { evidence };
        registry.validate()?;
        Ok(registry)
    }

    pub fn validate(&self) -> CitationAuditResult<()> {
        let mut ids = std::collections::BTreeSet::new();
        for entry in &self.evidence {
            entry.validate()?;
            if !ids.insert(&entry.evidence_id) {
                return Err(CitationAuditError::validation(
                    "evidence registry must not duplicate evidence IDs",
                ));
            }
        }
        Ok(())
    }

    pub fn resolve(&self, evidence_id: &str) -> Option<&ResolvedEvidence> {
        self.evidence
            .iter()
            .find(|entry| entry.evidence_id == evidence_id)
    }
}

/// A model or adapter reference. It carries only an evidence ID and an exact
/// quotation; locators and source text are never accepted from this value.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceReference {
    pub evidence_id: String,
    pub quotation: String,
}

impl EvidenceReference {
    pub fn validate_against(&self, registry: &EvidenceRegistry) -> CitationAuditResult<()> {
        require_non_empty("evidence_reference.evidence_id", &self.evidence_id)?;
        require_non_empty("evidence_reference.quotation", &self.quotation)?;
        let Some(resolved) = registry.resolve(&self.evidence_id) else {
            return Err(CitationAuditError::evidence_rejected(
                "evidence reference names an unknown server-resolved ID",
            ));
        };
        if !resolved.text.contains(&self.quotation) {
            return Err(CitationAuditError::evidence_rejected(
                "evidence reference quotation does not exactly occur in resolved evidence",
            ));
        }
        Ok(())
    }
}

/// Retrieval strategies a future adapter may record. The contract intentionally
/// does not choose or execute a live strategy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalStrategy {
    Exact,
    Lexical,
    Dense,
    Graph,
    Metadata,
}

/// Candidate identity returned during retrieval. It is not support until the
/// later server-resolution and exact-quotation validation step succeeds.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceCandidate {
    pub evidence_id: String,
    pub strategy: RetrievalStrategy,
}

impl EvidenceCandidate {
    pub fn validate(&self) -> CitationAuditResult<()> {
        require_non_empty("evidence_candidate.evidence_id", &self.evidence_id)
    }
}
