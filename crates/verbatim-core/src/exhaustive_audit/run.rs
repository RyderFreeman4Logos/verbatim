//! Persistable envelope for exhaustive-audit inputs, artifacts, cost, and status.

use serde::{Deserialize, Serialize};

use super::budget::{ExhaustiveAuditBudget, ExhaustiveAuditUsage};
use super::coverage::{
    establish_completeness, CompletenessStatus, CompletenessTarget, CoverageManifest,
};
use super::enumeration::CandidateEnumeration;
use super::error::{ExhaustiveAuditError, ExhaustiveAuditResult};
use super::scope::DeclaredAuditScope;
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

/// Canonical inputs that substantiate a persisted exhaustive status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExhaustiveAuditEvidence {
    pub scope: DeclaredAuditScope,
    pub coverage_manifest: CoverageManifest,
    pub enumerations: Vec<CandidateEnumeration>,
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
    pub exhaustive_evidence: Option<ExhaustiveAuditEvidence>,
    pub enumeration_hashes: Vec<String>,
    pub primary_enumeration_hash: Option<String>,
    pub primary_query_fingerprint: Option<String>,
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
            exhaustive_evidence: None,
            enumeration_hashes: Vec::new(),
            primary_enumeration_hash: None,
            primary_query_fingerprint: None,
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
        for record in &self.stage_records {
            require_digest("stage artifact_hash", &record.artifact_hash)?;
        }
        for hash in &self.enumeration_hashes {
            require_digest("enumeration_hash", hash)?;
        }
        for hash in [
            self.primary_enumeration_hash.as_deref(),
            self.primary_query_fingerprint.as_deref(),
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
        if self.status == CompletenessStatus::ExhaustiveOverDeclaredScope {
            self.require_bound_exhaustive_evidence()?;
        }
        Ok(())
    }

    fn require_bound_exhaustive_evidence(&self) -> ExhaustiveAuditResult<()> {
        let evidence = self.exhaustive_evidence.as_ref().ok_or_else(|| {
            ExhaustiveAuditError::validation("exhaustive status requires canonical evidence")
        })?;
        evidence.scope.validate()?;
        let scope_hash = super::content_hash_of(&evidence.scope)?;
        if self.scope_hash != scope_hash {
            return Err(ExhaustiveAuditError::validation(
                "exhaustive evidence scope must bind the run scope hash",
            ));
        }
        evidence
            .coverage_manifest
            .validate_for(&evidence.scope, &self.scope_hash)?;
        for enumeration in &evidence.enumerations {
            enumeration.validate()?;
        }
        if establish_completeness(
            self.target,
            &evidence.scope,
            &evidence.coverage_manifest,
            &evidence.enumerations,
        )? != CompletenessStatus::ExhaustiveOverDeclaredScope
        {
            return Err(ExhaustiveAuditError::validation(
                "exhaustive evidence must establish completeness",
            ));
        }
        let expected_enumeration_hashes = evidence
            .enumerations
            .iter()
            .map(super::content_hash_of)
            .collect::<ExhaustiveAuditResult<Vec<_>>>()?;
        if self.enumeration_hashes != expected_enumeration_hashes {
            return Err(ExhaustiveAuditError::validation(
                "enumeration hashes must bind canonical exhaustive evidence",
            ));
        }
        let primary_enumeration = evidence
            .enumerations
            .iter()
            .find(|enumeration| {
                enumeration.scope_hash == self.scope_hash && enumeration.is_deterministic_primary()
            })
            .ok_or_else(|| {
                ExhaustiveAuditError::validation(
                    "exhaustive evidence requires deterministic primary enumeration",
                )
            })?;
        let expected_primary_enumeration_hash = super::content_hash_of(primary_enumeration)?;
        let coverage_manifest_hash = required_digest(
            "exhaustive status requires a bound coverage manifest hash",
            self.coverage_manifest_hash.as_deref(),
        )?;
        if coverage_manifest_hash != super::content_hash_of(&evidence.coverage_manifest)? {
            return Err(ExhaustiveAuditError::validation(
                "coverage manifest hash must bind canonical exhaustive evidence",
            ));
        }
        let primary_enumeration_hash = required_digest(
            "exhaustive status requires a deterministic primary enumeration hash",
            self.primary_enumeration_hash.as_deref(),
        )?;
        if primary_enumeration_hash != expected_primary_enumeration_hash {
            return Err(ExhaustiveAuditError::validation(
                "primary enumeration hash must bind canonical exhaustive evidence",
            ));
        }
        let primary_query_fingerprint = required_digest(
            "exhaustive status requires a deterministic primary query fingerprint",
            self.primary_query_fingerprint.as_deref(),
        )?;
        if primary_query_fingerprint != primary_enumeration.query_fingerprint {
            return Err(ExhaustiveAuditError::validation(
                "primary query fingerprint must bind canonical exhaustive evidence",
            ));
        }
        let expected_query_fingerprints = evidence
            .enumerations
            .iter()
            .map(|enumeration| enumeration.query_fingerprint.clone())
            .collect::<Vec<_>>();
        if self.query_fingerprints != expected_query_fingerprints {
            return Err(ExhaustiveAuditError::validation(
                "query fingerprints must bind canonical exhaustive evidence",
            ));
        }
        let reconciliation_hash = required_digest(
            "exhaustive status requires a bound reconciliation hash",
            self.reconciliation_hash.as_deref(),
        )?;
        let expected_reconciliation_hash = super::content_hash_of(&(
            &evidence.scope,
            &evidence.coverage_manifest,
            &evidence.enumerations,
        ))?;
        if reconciliation_hash != expected_reconciliation_hash {
            return Err(ExhaustiveAuditError::validation(
                "reconciliation hash must bind canonical exhaustive evidence",
            ));
        }
        let report_hash = required_digest(
            "exhaustive status requires a bound report hash",
            self.report_hash.as_deref(),
        )?;
        let expected_report_hash = super::content_hash_of(&(
            self.target,
            &evidence.scope,
            &evidence.coverage_manifest,
            &evidence.enumerations,
        ))?;
        if report_hash != expected_report_hash {
            return Err(ExhaustiveAuditError::validation(
                "report hash must bind canonical exhaustive evidence",
            ));
        }
        let expected_stage_records = [
            (AuditStage::Declared, self.scope_hash.as_str()),
            (AuditStage::Enumerating, primary_enumeration_hash),
            (AuditStage::Covering, coverage_manifest_hash),
            (AuditStage::Reconciling, reconciliation_hash),
            (AuditStage::Reporting, report_hash),
        ];
        if self.stage_records.len() != expected_stage_records.len()
            || !self.stage_records.iter().zip(expected_stage_records).all(
                |(record, (stage, artifact_hash))| {
                    record.stage == stage && record.artifact_hash == artifact_hash
                },
            )
        {
            return Err(ExhaustiveAuditError::validation(
                "exhaustive status requires complete bound stage evidence",
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

fn required_digest<'a>(field: &str, value: Option<&'a str>) -> ExhaustiveAuditResult<&'a str> {
    let value = value.ok_or_else(|| ExhaustiveAuditError::validation(field))?;
    require_digest(field, value)?;
    Ok(value)
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
