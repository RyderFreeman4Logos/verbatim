//! Claim classifications, deterministic validation, and aggregate coverage.

use serde::{Deserialize, Serialize};

use super::util::{content_hash_of, require_non_empty};
use super::{
    CitationAuditError, CitationAuditResult, ClaimRecord, ClaimSegmentation, EvidenceReference,
    EvidenceRegistry,
};

/// Fixed output taxonomy. A model may propose one of these labels, but only
/// `validate_for_claim` can make it an evidence-backed audit result.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceClassification {
    Supported,
    PartiallySupported,
    Contradicted,
    Unrelated,
    Insufficient,
}

impl EvidenceClassification {
    pub fn all() -> &'static [Self] {
        &[
            Self::Supported,
            Self::PartiallySupported,
            Self::Contradicted,
            Self::Unrelated,
            Self::Insufficient,
        ]
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceApplicability {
    Applicable,
    PartiallyApplicable,
    NotApplicable,
    Unknown,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CalibrationStatus {
    Calibrated,
    Uncalibrated,
    NotAvailable,
}

/// An integer calibration value avoids lossy floating-point persistence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Calibration {
    pub status: CalibrationStatus,
    pub basis_points: u16,
}

impl Calibration {
    fn validate(&self) -> CitationAuditResult<()> {
        if self.basis_points > 10_000 {
            return Err(CitationAuditError::validation(
                "calibration basis_points must not exceed 10000",
            ));
        }
        Ok(())
    }
}

/// A source-backed conflict which keeps the reason for contradiction visible.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimConflict {
    pub evidence_id: String,
    pub detail: String,
}

impl ClaimConflict {
    fn validate(&self, evidence_ids: &std::collections::BTreeSet<&str>) -> CitationAuditResult<()> {
        require_non_empty("claim_conflict.evidence_id", &self.evidence_id)?;
        require_non_empty("claim_conflict.detail", &self.detail)?;
        if !evidence_ids.contains(self.evidence_id.as_str()) {
            return Err(CitationAuditError::validation(
                "claim conflict must name an evidence reference in the same result",
            ));
        }
        Ok(())
    }
}

/// Per-claim classification proposed by an adapter and then validated against
/// trusted server evidence. It has no locator field by design.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimAuditResult {
    pub claim_id: String,
    pub classification: EvidenceClassification,
    pub evidence: Vec<EvidenceReference>,
    pub missing_requirements: Vec<String>,
    pub conflicts: Vec<ClaimConflict>,
    pub source_applicability: SourceApplicability,
    pub confidence: Calibration,
}

#[derive(Clone, PartialEq, Eq)]
pub struct ClaimAuditResultFields {
    pub claim_id: String,
    pub classification: EvidenceClassification,
    pub evidence: Vec<EvidenceReference>,
    pub missing_requirements: Vec<String>,
    pub conflicts: Vec<ClaimConflict>,
    pub source_applicability: SourceApplicability,
    pub confidence: Calibration,
}

impl ClaimAuditResult {
    pub fn new(fields: ClaimAuditResultFields) -> CitationAuditResult<Self> {
        let result = Self {
            claim_id: fields.claim_id,
            classification: fields.classification,
            evidence: fields.evidence,
            missing_requirements: fields.missing_requirements,
            conflicts: fields.conflicts,
            source_applicability: fields.source_applicability,
            confidence: fields.confidence,
        };
        result.validate_shape()?;
        Ok(result)
    }

    pub fn validate_shape(&self) -> CitationAuditResult<()> {
        require_non_empty("claim_audit_result.claim_id", &self.claim_id)?;
        self.confidence.validate()?;
        for requirement in &self.missing_requirements {
            require_non_empty("claim_audit_result.missing_requirement", requirement)?;
        }
        let evidence_ids: std::collections::BTreeSet<_> = self
            .evidence
            .iter()
            .map(|reference| reference.evidence_id.as_str())
            .collect();
        if evidence_ids.len() != self.evidence.len() {
            return Err(CitationAuditError::validation(
                "claim audit result must not duplicate evidence IDs",
            ));
        }
        for conflict in &self.conflicts {
            conflict.validate(&evidence_ids)?;
        }
        match self.classification {
            EvidenceClassification::Supported => {
                if self.evidence.is_empty()
                    || !self.missing_requirements.is_empty()
                    || !self.conflicts.is_empty()
                    || self.source_applicability != SourceApplicability::Applicable
                {
                    return Err(CitationAuditError::validation(
                        "supported results require evidence, applicability, and no gaps or conflicts",
                    ));
                }
            }
            EvidenceClassification::PartiallySupported => {
                if self.evidence.is_empty()
                    || self.missing_requirements.is_empty()
                    || self.source_applicability != SourceApplicability::PartiallyApplicable
                {
                    return Err(CitationAuditError::validation(
                        "partially supported results require evidence, gaps, and partial applicability",
                    ));
                }
            }
            EvidenceClassification::Contradicted => {
                if self.evidence.is_empty()
                    || self.conflicts.is_empty()
                    || self.source_applicability != SourceApplicability::Applicable
                {
                    return Err(CitationAuditError::validation(
                        "contradicted results require evidence, an explicit conflict, and applicability",
                    ));
                }
            }
            EvidenceClassification::Unrelated => {
                if self.evidence.is_empty()
                    || self.missing_requirements.is_empty()
                    || !self.conflicts.is_empty()
                    || self.source_applicability != SourceApplicability::NotApplicable
                {
                    return Err(CitationAuditError::validation(
                        "unrelated results require evidence, a stated gap, and non-applicability",
                    ));
                }
            }
            EvidenceClassification::Insufficient => {
                if !self.evidence.is_empty()
                    || self.missing_requirements.is_empty()
                    || !self.conflicts.is_empty()
                    || self.source_applicability != SourceApplicability::Unknown
                {
                    return Err(CitationAuditError::validation(
                        "insufficient results require explicit gaps and no unsupported evidence",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Enforce exact claim identity, server-known evidence IDs, and verbatim
    /// quotation membership. Existing external citations do not participate.
    pub fn validate_for_claim(
        &self,
        claim: &ClaimRecord,
        registry: &EvidenceRegistry,
    ) -> CitationAuditResult<()> {
        self.validate_shape()?;
        if self.claim_id != claim.claim_id.as_str() {
            return Err(CitationAuditError::validation(
                "claim audit result must bind to its exact claim ID",
            ));
        }
        registry.validate()?;
        for reference in &self.evidence {
            reference.validate_against(registry)?;
        }
        Ok(())
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    Complete,
    Incomplete,
    Blocked,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimCoverageCounts {
    pub supported: u64,
    pub partially_supported: u64,
    pub contradicted: u64,
    pub unrelated: u64,
    pub insufficient: u64,
}

/// Aggregate envelope for persistable claim-level audit artifacts.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClaimCoverageEnvelope {
    pub document_hash: String,
    pub segmentation_hash: String,
    pub results_hash: String,
    pub total_claims: u64,
    pub audited_claims: u64,
    pub counts: ClaimCoverageCounts,
    pub status: CoverageStatus,
}

impl ClaimCoverageEnvelope {
    pub fn new(
        segmentation: &ClaimSegmentation,
        results: &[ClaimAuditResult],
        registry: &EvidenceRegistry,
    ) -> CitationAuditResult<Self> {
        registry.validate()?;
        if segmentation.claims.len() != results.len() {
            return Err(CitationAuditError::validation(
                "coverage requires exactly one result for every segmented claim",
            ));
        }
        let mut results_by_claim = std::collections::BTreeMap::new();
        for result in results {
            if results_by_claim
                .insert(result.claim_id.as_str(), result)
                .is_some()
            {
                return Err(CitationAuditError::validation(
                    "coverage must not duplicate claim results",
                ));
            }
        }
        let mut counts = ClaimCoverageCounts::default();
        for claim in &segmentation.claims {
            let Some(result) = results_by_claim.get(claim.claim_id.as_str()) else {
                return Err(CitationAuditError::validation(
                    "coverage result is missing a segmented claim",
                ));
            };
            result.validate_for_claim(claim, registry)?;
            match result.classification {
                EvidenceClassification::Supported => counts.supported += 1,
                EvidenceClassification::PartiallySupported => counts.partially_supported += 1,
                EvidenceClassification::Contradicted => counts.contradicted += 1,
                EvidenceClassification::Unrelated => counts.unrelated += 1,
                EvidenceClassification::Insufficient => counts.insufficient += 1,
            }
        }
        let total_claims = u64::try_from(segmentation.claims.len())
            .map_err(|_| CitationAuditError::validation("claim count exceeds u64"))?;
        Ok(Self {
            document_hash: segmentation.document_hash.clone(),
            segmentation_hash: content_hash_of(segmentation)?,
            results_hash: content_hash_of(results)?,
            total_claims,
            audited_claims: total_claims,
            counts,
            status: CoverageStatus::Complete,
        })
    }

    pub fn validate_for(
        &self,
        segmentation: &ClaimSegmentation,
        results: &[ClaimAuditResult],
        registry: &EvidenceRegistry,
    ) -> CitationAuditResult<()> {
        let expected = Self::new(segmentation, results, registry)?;
        if self != &expected {
            return Err(CitationAuditError::validation(
                "coverage envelope must exactly bind segmentation and validated results",
            ));
        }
        Ok(())
    }
}
