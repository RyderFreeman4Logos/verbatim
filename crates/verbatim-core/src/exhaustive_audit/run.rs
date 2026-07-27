//! Persistable envelope for exhaustive-audit inputs, artifacts, cost, and status.

use serde::{Deserialize, Serialize};

use super::budget::{ExhaustiveAuditBudget, ExhaustiveAuditUsage};
use super::coverage::{CompletenessStatus, CompletenessTarget};
use super::error::{ExhaustiveAuditError, ExhaustiveAuditResult};
use super::stage::AuditStage;
use super::util::{require_digest, require_non_empty};

pub const EXHAUSTIVE_AUDIT_WORKFLOW_SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditStageRecord {
    pub stage: AuditStage,
    pub artifact_hash: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditWarning {
    pub code: String,
    pub detail: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditWorkflowRun {
    pub schema_version: u32,
    pub run_id: String,
    pub scope_hash: String,
    pub target: CompletenessTarget,
    pub current_stage: AuditStage,
    pub status: CompletenessStatus,
    pub budget: ExhaustiveAuditBudget,
    pub usage: ExhaustiveAuditUsage,
    pub stage_records: Vec<AuditStageRecord>,
    pub enumeration_hashes: Vec<String>,
    pub coverage_manifest_hash: Option<String>,
    pub reconciliation_hash: Option<String>,
    pub report_hash: Option<String>,
    pub query_fingerprints: Vec<String>,
    pub warnings: Vec<AuditWarning>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditWorkflowRunFields {
    pub run_id: String,
    pub scope_hash: String,
    pub target: CompletenessTarget,
    pub budget: ExhaustiveAuditBudget,
}

impl AuditWorkflowRun {
    pub fn new(fields: AuditWorkflowRunFields) -> ExhaustiveAuditResult<Self> {
        require_non_empty("run_id", &fields.run_id)?;
        require_digest("scope_hash", &fields.scope_hash)?;
        let run = Self {
            schema_version: EXHAUSTIVE_AUDIT_WORKFLOW_SCHEMA_VERSION,
            run_id: fields.run_id,
            scope_hash: fields.scope_hash,
            target: fields.target,
            current_stage: AuditStage::Declared,
            status: CompletenessStatus::UnableToEstablish,
            budget: fields.budget,
            usage: ExhaustiveAuditUsage::default(),
            stage_records: Vec::new(),
            enumeration_hashes: Vec::new(),
            coverage_manifest_hash: None,
            reconciliation_hash: None,
            report_hash: None,
            query_fingerprints: Vec::new(),
            warnings: Vec::new(),
        };
        run.validate()?;
        Ok(run)
    }

    pub fn validate(&self) -> ExhaustiveAuditResult<()> {
        if self.schema_version != EXHAUSTIVE_AUDIT_WORKFLOW_SCHEMA_VERSION {
            return Err(ExhaustiveAuditError::validation(format!(
                "unknown exhaustive-audit schema_version {}",
                self.schema_version
            )));
        }
        require_non_empty("run_id", &self.run_id)?;
        require_digest("scope_hash", &self.scope_hash)?;
        for hash in &self.enumeration_hashes {
            require_digest("enumeration_hash", hash)?;
        }
        for hash in [
            self.coverage_manifest_hash.as_deref(),
            self.reconciliation_hash.as_deref(),
            self.report_hash.as_deref(),
        ]
        .into_iter()
        .flatten()
        {
            require_digest("audit artifact hash", hash)?;
        }
        for fingerprint in &self.query_fingerprints {
            require_digest("query_fingerprint", fingerprint)?;
        }
        for warning in &self.warnings {
            require_non_empty("warning.code", &warning.code)?;
            require_non_empty("warning.detail", &warning.detail)?;
        }
        let terminal_status_stage = match self.status {
            CompletenessStatus::ExhaustiveOverDeclaredScope => AuditStage::Complete,
            CompletenessStatus::Incomplete => AuditStage::Incomplete,
            CompletenessStatus::UnableToEstablish => AuditStage::UnableToEstablish,
            CompletenessStatus::Blocked => AuditStage::Blocked,
        };
        if self.current_stage.is_terminal() && self.current_stage != terminal_status_stage {
            return Err(ExhaustiveAuditError::validation(
                "terminal audit stage must match completeness status",
            ));
        }
        if self.status == CompletenessStatus::ExhaustiveOverDeclaredScope
            && self.report_hash.is_none()
        {
            return Err(ExhaustiveAuditError::validation(
                "exhaustive status requires a bound report hash",
            ));
        }
        Ok(())
    }

    pub fn record_stage(&mut self, record: AuditStageRecord) -> ExhaustiveAuditResult<()> {
        if self.current_stage.is_terminal() || record.stage != self.current_stage {
            return Err(ExhaustiveAuditError::IllegalTransition {
                detail: "stage record must match an active non-terminal stage".into(),
            });
        }
        require_digest("stage artifact_hash", &record.artifact_hash)?;
        self.stage_records.push(record);
        Ok(())
    }

    pub fn add_usage(&mut self, increment: ExhaustiveAuditUsage) -> ExhaustiveAuditResult<()> {
        self.usage = self.usage.checked_add(&increment, &self.budget)?;
        Ok(())
    }
}

/// Decode a run and reject unknown schemas or inconsistent terminal status.
pub fn decode_audit_workflow_run_json(bytes: &[u8]) -> ExhaustiveAuditResult<AuditWorkflowRun> {
    let run: AuditWorkflowRun = serde_json::from_slice(bytes)
        .map_err(|err| ExhaustiveAuditError::validation(format!("audit run JSON decode: {err}")))?;
    run.validate()?;
    Ok(run)
}

/// Encode a validated run as compact JSON for persistence adapters.
pub fn encode_audit_workflow_run_json(run: &AuditWorkflowRun) -> ExhaustiveAuditResult<Vec<u8>> {
    run.validate()?;
    serde_json::to_vec(run)
        .map_err(|err| ExhaustiveAuditError::validation(format!("audit run JSON encode: {err}")))
}
