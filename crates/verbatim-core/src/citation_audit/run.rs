//! Persistable per-claim audit run envelope and JSON binding.

use serde::{Deserialize, Serialize};

use super::util::{content_hash_of, require_non_empty};
use super::{
    AuditDocument, CitationAuditBudget, CitationAuditError, CitationAuditResult,
    CitationAuditStage, CitationAuditUsage, ClaimAuditResult, ClaimCoverageEnvelope,
    ClaimSegmentation, EvidenceRegistry,
};

pub const CITATION_AUDIT_WORKFLOW_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CitationAuditRunStatus {
    Running,
    Complete,
    Incomplete,
    Disabled,
}

impl CitationAuditRunStatus {
    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::Running)
    }
}

/// Versioned persistence envelope. It records only hashes for text-bearing
/// artifacts, avoiding duplicate source/evidence text in run metadata.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationAuditRun {
    pub schema_version: u32,
    pub run_id: String,
    pub document_hash: String,
    pub current_stage: CitationAuditStage,
    pub status: CitationAuditRunStatus,
    pub budget: CitationAuditBudget,
    pub usage: CitationAuditUsage,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub segmentation_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub results_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_hash: Option<String>,
}

impl CitationAuditRun {
    pub fn new(
        run_id: String,
        document: &AuditDocument,
        budget: CitationAuditBudget,
    ) -> CitationAuditResult<Self> {
        require_non_empty("citation_audit_run.run_id", &run_id)?;
        CitationAuditBudget::new(super::CitationAuditBudgetFields {
            max_claims: budget.max_claims,
            max_candidates: budget.max_candidates,
            max_classifications: budget.max_classifications,
            max_cost_units: budget.max_cost_units,
            max_wall_time_ms: budget.max_wall_time_ms,
        })?;
        Ok(Self {
            schema_version: CITATION_AUDIT_WORKFLOW_SCHEMA_VERSION,
            run_id,
            document_hash: document.content_hash()?,
            current_stage: CitationAuditStage::Segmenting,
            status: CitationAuditRunStatus::Running,
            budget,
            usage: CitationAuditUsage::default(),
            segmentation_hash: None,
            results_hash: None,
            coverage_hash: None,
        })
    }

    pub fn validate(&self) -> CitationAuditResult<()> {
        if self.schema_version != CITATION_AUDIT_WORKFLOW_SCHEMA_VERSION {
            return Err(CitationAuditError::validation(
                "unknown citation-audit run schema version",
            ));
        }
        require_non_empty("citation_audit_run.run_id", &self.run_id)?;
        super::util::require_sha256("citation_audit_run.document_hash", &self.document_hash)?;
        for value in [
            &self.segmentation_hash,
            &self.results_hash,
            &self.coverage_hash,
        ]
        .into_iter()
        .flatten()
        {
            super::util::require_sha256("citation_audit_run.artifact_hash", value)?;
        }
        if self.status.is_terminal() != self.current_stage.is_terminal() {
            return Err(CitationAuditError::validation(
                "run status and current stage must agree on terminality",
            ));
        }
        if self.status == CitationAuditRunStatus::Complete
            && (self.segmentation_hash.is_none()
                || self.results_hash.is_none()
                || self.coverage_hash.is_none())
        {
            return Err(CitationAuditError::validation(
                "complete run requires segmentation, result, and coverage hashes",
            ));
        }
        Ok(())
    }

    pub fn record_usage(&mut self, increment: CitationAuditUsage) -> CitationAuditResult<()> {
        self.usage = self.usage.checked_add(increment, &self.budget)?;
        Ok(())
    }
}

pub fn encode_citation_audit_run_json(run: &CitationAuditRun) -> CitationAuditResult<Vec<u8>> {
    run.validate()?;
    serde_json::to_vec(run)
        .map_err(|_| CitationAuditError::validation("citation-audit run cannot be serialized"))
}

pub fn decode_citation_audit_run_json(bytes: &[u8]) -> CitationAuditResult<CitationAuditRun> {
    let run: CitationAuditRun = serde_json::from_slice(bytes)
        .map_err(|_| CitationAuditError::validation("citation-audit run JSON is malformed"))?;
    run.validate()?;
    Ok(run)
}

pub fn complete_run(
    run: &mut CitationAuditRun,
    segmentation: &ClaimSegmentation,
    results: &[ClaimAuditResult],
    coverage: &ClaimCoverageEnvelope,
    registry: &EvidenceRegistry,
) -> CitationAuditResult<()> {
    if run.current_stage != CitationAuditStage::Aggregating
        || run.status != CitationAuditRunStatus::Running
    {
        return Err(CitationAuditError::IllegalTransition {
            from: run.current_stage,
            to: CitationAuditStage::Complete,
        });
    }
    coverage.validate_for(segmentation, results, registry)?;
    if run.document_hash != segmentation.document_hash
        || run.document_hash != coverage.document_hash
    {
        return Err(CitationAuditError::validation(
            "run, segmentation, and coverage must bind the same document hash",
        ));
    }
    run.segmentation_hash = Some(content_hash_of(segmentation)?);
    run.results_hash = Some(content_hash_of(results)?);
    run.coverage_hash = Some(content_hash_of(coverage)?);
    run.current_stage = CitationAuditStage::Complete;
    run.status = CitationAuditRunStatus::Complete;
    run.validate()
}
