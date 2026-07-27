//! WorkflowRun persistence envelope: stages, hashes, fingerprints, costs.

use serde::{Deserialize, Serialize};

use super::answer::GroundedAnswer;
use super::claim::{require_digest, require_non_empty};
use super::error::{WorkflowError, WorkflowResult};
use super::stage::WorkflowStage;
use crate::wire_schemas::{encode_wire_document, wire_content_hash};

/// Schema version for WorkflowRun documents. Unknown versions fail closed.
pub const GROUNDED_ANSWER_WORKFLOW_SCHEMA_VERSION: u32 = 1;

/// Final status of a completed (or failed-closed) workflow run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowFinalStatus {
    /// Published a GroundedAnswer with only verified claims.
    Published,
    /// Typed abstention (model/policy/verify failure).
    Abstained,
    /// Workflow disabled; R/RA path should be used instead.
    Disabled,
    /// Still running (not terminal).
    InProgress,
}

impl WorkflowFinalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Published => "published",
            Self::Abstained => "abstained",
            Self::Disabled => "disabled",
            Self::InProgress => "in_progress",
        }
    }

    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::InProgress)
    }
}

/// Severity for non-fatal workflow warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowWarningSeverity {
    Info,
    Warning,
    /// Serious but did not force abstention by itself.
    Error,
}

impl WorkflowWarningSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// Non-fatal warning recorded on a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowWarning {
    pub severity: WorkflowWarningSeverity,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stage: Option<WorkflowStage>,
}

impl WorkflowWarning {
    pub fn new(
        severity: WorkflowWarningSeverity,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> WorkflowResult<Self> {
        let w = Self {
            severity,
            code: code.into(),
            message: message.into(),
            stage: None,
        };
        w.validate()?;
        Ok(w)
    }

    pub fn validate(&self) -> WorkflowResult<()> {
        require_non_empty("warning.code", &self.code)?;
        require_non_empty("warning.message", &self.message)?;
        Ok(())
    }
}

/// Opaque cost accounting for a run (tokens / units; no currency).
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct WorkflowCost {
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub input_tokens: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub output_tokens: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub model_calls: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub cost_units: u64,
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

impl WorkflowCost {
    pub fn saturating_add(&self, other: &Self) -> Self {
        Self {
            input_tokens: self.input_tokens.saturating_add(other.input_tokens),
            output_tokens: self.output_tokens.saturating_add(other.output_tokens),
            model_calls: self.model_calls.saturating_add(other.model_calls),
            cost_units: self.cost_units.saturating_add(other.cost_units),
        }
    }
}

/// Per-stage record inside a WorkflowRun.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowStageRecord {
    pub stage: WorkflowStage,
    /// Content hash of the primary artifact produced at this stage (if any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_hash: Option<String>,
    /// Opaque model fingerprint used during this stage (if any).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost: Option<WorkflowCost>,
    /// True when the stage completed successfully (vs failed into abstention).
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

impl WorkflowStageRecord {
    pub fn validate(&self) -> WorkflowResult<()> {
        if let Some(h) = &self.artifact_hash {
            require_digest("stage.artifact_hash", h)?;
        }
        if let Some(m) = &self.model_fingerprint {
            require_non_empty("stage.model_fingerprint", m)?;
        }
        if let Some(d) = &self.detail {
            require_non_empty("stage.detail", d)?;
        }
        Ok(())
    }
}

/// Persistence envelope for one grounded-answer workflow execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRun {
    /// Document schema version (must equal [`GROUNDED_ANSWER_WORKFLOW_SCHEMA_VERSION`]).
    pub schema_version: u32,
    pub run_id: String,
    pub query_plan_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_pack_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_pack_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub answer_plan_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub draft_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub grounded_answer_hash: Option<String>,
    pub current_stage: WorkflowStage,
    pub final_status: WorkflowFinalStatus,
    pub stages: Vec<WorkflowStageRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<WorkflowWarning>,
    #[serde(default)]
    pub total_cost: WorkflowCost,
    /// Optional abstention reason (required when status is Abstained).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub abstention_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub model_fingerprint: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
}

/// Field bundle for [`WorkflowRun::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRunFields {
    pub run_id: String,
    pub query_plan_hash: String,
    pub profile_ref: Option<String>,
    pub generation: Option<String>,
}

impl WorkflowRun {
    /// Start a new in-progress run bound to a QueryPlan hash.
    pub fn new(fields: WorkflowRunFields) -> WorkflowResult<Self> {
        let run = Self {
            schema_version: GROUNDED_ANSWER_WORKFLOW_SCHEMA_VERSION,
            run_id: fields.run_id,
            query_plan_hash: fields.query_plan_hash,
            evidence_pack_hash: None,
            context_pack_hash: None,
            answer_plan_hash: None,
            draft_hash: None,
            grounded_answer_hash: None,
            current_stage: WorkflowStage::Planned,
            final_status: WorkflowFinalStatus::InProgress,
            stages: vec![WorkflowStageRecord {
                stage: WorkflowStage::Planned,
                artifact_hash: None,
                model_fingerprint: None,
                cost: None,
                ok: true,
                detail: None,
            }],
            warnings: Vec::new(),
            total_cost: WorkflowCost::default(),
            abstention_reason: None,
            model_fingerprint: None,
            profile_ref: fields.profile_ref,
            generation: fields.generation,
        };
        run.validate()?;
        Ok(run)
    }

    pub fn validate(&self) -> WorkflowResult<()> {
        if self.schema_version != GROUNDED_ANSWER_WORKFLOW_SCHEMA_VERSION {
            return Err(WorkflowError::validation(format!(
                "unsupported grounded-answer workflow schema version {}; expected {}",
                self.schema_version, GROUNDED_ANSWER_WORKFLOW_SCHEMA_VERSION
            )));
        }
        require_non_empty("run_id", &self.run_id)?;
        require_digest("query_plan_hash", &self.query_plan_hash)?;
        if let Some(h) = &self.evidence_pack_hash {
            require_digest("evidence_pack_hash", h)?;
        }
        if let Some(h) = &self.context_pack_hash {
            require_digest("context_pack_hash", h)?;
        }
        if let Some(h) = &self.answer_plan_hash {
            require_digest("answer_plan_hash", h)?;
        }
        if let Some(h) = &self.draft_hash {
            require_digest("draft_hash", h)?;
        }
        if let Some(h) = &self.grounded_answer_hash {
            require_digest("grounded_answer_hash", h)?;
        }
        if self.stages.is_empty() {
            return Err(WorkflowError::validation(
                "workflow run requires at least one stage record",
            ));
        }
        for stage in &self.stages {
            stage.validate()?;
        }
        for w in &self.warnings {
            w.validate()?;
        }
        if let Some(m) = &self.model_fingerprint {
            require_non_empty("model_fingerprint", m)?;
        }
        if let Some(p) = &self.profile_ref {
            require_non_empty("profile_ref", p)?;
        }
        if let Some(g) = &self.generation {
            require_non_empty("generation", g)?;
        }
        match self.final_status {
            WorkflowFinalStatus::Published => {
                if self.current_stage != WorkflowStage::Published {
                    return Err(WorkflowError::validation(
                        "published status requires current_stage=published",
                    ));
                }
                if self.grounded_answer_hash.is_none() {
                    return Err(WorkflowError::validation(
                        "published status requires grounded_answer_hash",
                    ));
                }
            }
            WorkflowFinalStatus::Abstained => {
                if self.current_stage != WorkflowStage::Abstained {
                    return Err(WorkflowError::validation(
                        "abstained status requires current_stage=abstained",
                    ));
                }
                match &self.abstention_reason {
                    Some(r) => require_non_empty("abstention_reason", r)?,
                    None => {
                        return Err(WorkflowError::validation(
                            "abstained status requires abstention_reason",
                        ));
                    }
                }
            }
            WorkflowFinalStatus::Disabled => {
                // Disabled may leave stage at Planned.
            }
            WorkflowFinalStatus::InProgress => {
                if self.current_stage.is_terminal() {
                    return Err(WorkflowError::validation(
                        "in_progress status cannot use a terminal stage",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Record a successful stage advance with optional artifact hash / cost.
    ///
    /// Fail-closed: `record.stage` must be a legal successor of
    /// [`Self::current_stage`] (or `Abstained` from any non-terminal). Same-stage
    /// re-records are allowed only for [`WorkflowStage::Generating`] (plan then
    /// draft). Terminal stages reject further records. Published is finalized
    /// only via [`Self::publish`].
    pub fn record_stage(&mut self, record: WorkflowStageRecord) -> WorkflowResult<()> {
        record.validate()?;
        if self.current_stage.is_terminal() || self.final_status.is_terminal() {
            return Err(WorkflowError::illegal_transition(
                self.current_stage,
                record.stage,
                "terminal stage cannot record further advances",
            ));
        }
        if !is_legal_stage_record(self.current_stage, record.stage) {
            return Err(WorkflowError::illegal_transition(
                self.current_stage,
                record.stage,
                format!(
                    "stage {} is not a legal successor of {}",
                    record.stage.as_str(),
                    self.current_stage.as_str()
                ),
            ));
        }
        if let Some(cost) = &record.cost {
            self.total_cost = self.total_cost.saturating_add(cost);
        }
        if let Some(m) = &record.model_fingerprint {
            self.model_fingerprint = Some(m.clone());
        }
        match record.stage {
            WorkflowStage::Retrieving => {
                if let Some(h) = &record.artifact_hash {
                    self.evidence_pack_hash = Some(h.clone());
                }
            }
            WorkflowStage::Assembling => {
                if let Some(h) = &record.artifact_hash {
                    self.context_pack_hash = Some(h.clone());
                }
            }
            WorkflowStage::Generating => {
                if let Some(h) = &record.artifact_hash {
                    // First generating record is answer plan; subsequent may be draft.
                    if self.answer_plan_hash.is_none() {
                        self.answer_plan_hash = Some(h.clone());
                    } else {
                        self.draft_hash = Some(h.clone());
                    }
                }
            }
            WorkflowStage::Abstained => {
                // Allow terminal abstain from any non-terminal via record_stage.
                let reason = record
                    .detail
                    .clone()
                    .filter(|d| !d.trim().is_empty())
                    .unwrap_or_else(|| "abstained".into());
                self.final_status = WorkflowFinalStatus::Abstained;
                self.abstention_reason = Some(reason);
            }
            _ => {}
        }
        self.current_stage = record.stage;
        self.stages.push(record);
        self.validate()
    }

    /// Fail closed into typed abstention.
    pub fn abstain(&mut self, reason: impl Into<String>) -> WorkflowResult<()> {
        let reason = reason.into();
        require_non_empty("abstention_reason", &reason)?;
        self.current_stage = WorkflowStage::Abstained;
        self.final_status = WorkflowFinalStatus::Abstained;
        self.abstention_reason = Some(reason.clone());
        self.stages.push(WorkflowStageRecord {
            stage: WorkflowStage::Abstained,
            artifact_hash: None,
            model_fingerprint: None,
            cost: None,
            ok: false,
            detail: Some(reason),
        });
        self.validate()
    }

    /// Mark the run disabled without a verified answer.
    pub fn mark_disabled(&mut self, detail: impl Into<String>) -> WorkflowResult<()> {
        let detail = detail.into();
        require_non_empty("disabled.detail", &detail)?;
        self.final_status = WorkflowFinalStatus::Disabled;
        self.warnings.push(WorkflowWarning {
            severity: WorkflowWarningSeverity::Info,
            code: "workflow_disabled".into(),
            message: detail,
            stage: Some(self.current_stage),
        });
        self.validate()
    }

    /// Finalize as published with a grounded answer content hash.
    ///
    /// Requires [`WorkflowStage::Rendering`]. Binds the answer digests to this
    /// run: `query_plan_hash` must match, and when the run already carries a
    /// `context_pack_hash` it must equal the answer's.
    pub fn publish(&mut self, grounded_answer: &GroundedAnswer) -> WorkflowResult<()> {
        grounded_answer.validate()?;
        if self.current_stage != WorkflowStage::Rendering {
            return Err(WorkflowError::illegal_transition(
                self.current_stage,
                WorkflowStage::Published,
                "publish requires rendering stage",
            ));
        }
        bind_answer_digests_to_run(self, grounded_answer)?;
        let hash = content_hash_of(grounded_answer)?;
        self.grounded_answer_hash = Some(hash.clone());
        self.context_pack_hash = Some(grounded_answer.context_pack_hash.clone());
        self.model_fingerprint = Some(grounded_answer.model_fingerprint.clone());
        self.current_stage = WorkflowStage::Published;
        self.final_status = WorkflowFinalStatus::Published;
        self.stages.push(WorkflowStageRecord {
            stage: WorkflowStage::Published,
            artifact_hash: Some(hash),
            model_fingerprint: Some(grounded_answer.model_fingerprint.clone()),
            cost: None,
            ok: true,
            detail: None,
        });
        self.validate()
    }

    /// Content hash of this run document (for persistence / audit).
    pub fn content_hash(&self) -> WorkflowResult<String> {
        content_hash_of(self)
    }
}

/// Decode a WorkflowRun from JSON and fail closed on unknown schema / invalid fields.
pub fn decode_workflow_run_json(bytes: &[u8]) -> WorkflowResult<WorkflowRun> {
    let value: WorkflowRun = serde_json::from_slice(bytes)
        .map_err(|err| WorkflowError::validation(format!("workflow run JSON decode: {err}")))?;
    value.validate()?;
    Ok(value)
}

/// Encode a WorkflowRun to compact JSON bytes.
pub fn encode_workflow_run_json(run: &WorkflowRun) -> WorkflowResult<Vec<u8>> {
    run.validate()?;
    encode_wire_document(run)
        .map_err(|err| WorkflowError::validation(format!("workflow run JSON encode: {err}")))
}

/// Stable content hash of a serializable workflow artifact.
pub fn content_hash_of<T: Serialize>(value: &T) -> WorkflowResult<String> {
    let bytes = encode_wire_document(value)
        .map_err(|err| WorkflowError::validation(format!("content hash encode: {err}")))?;
    Ok(wire_content_hash(&bytes))
}

/// Legal `record_stage` successors (mirrors [`super::workflow::advance_stage`]).
///
/// - `Abstained` is allowed from any non-terminal stage.
/// - `Generating` may re-record itself (answer plan then draft).
/// - `Published` is not a record_stage successor; use [`WorkflowRun::publish`].
fn is_legal_stage_record(current: WorkflowStage, next: WorkflowStage) -> bool {
    if next == WorkflowStage::Abstained {
        return !current.is_terminal();
    }
    matches!(
        (current, next),
        (WorkflowStage::Planned, WorkflowStage::Retrieving)
            | (WorkflowStage::Retrieving, WorkflowStage::Assembling)
            | (WorkflowStage::Assembling, WorkflowStage::Generating)
            | (WorkflowStage::Generating, WorkflowStage::Generating)
            | (WorkflowStage::Generating, WorkflowStage::Verifying)
            | (WorkflowStage::Verifying, WorkflowStage::Rendering)
            | (WorkflowStage::Verifying, WorkflowStage::Generating)
    )
}

/// Fail closed when a GroundedAnswer is not bound to this run's digests.
pub(crate) fn bind_answer_digests_to_run(
    run: &WorkflowRun,
    answer: &GroundedAnswer,
) -> WorkflowResult<()> {
    if answer.query_plan_hash != run.query_plan_hash {
        return Err(WorkflowError::validation(
            "answer query_plan_hash does not match run query_plan_hash",
        ));
    }
    if let Some(run_cp) = &run.context_pack_hash {
        if run_cp != &answer.context_pack_hash {
            return Err(WorkflowError::validation(
                "answer context_pack_hash does not match run context_pack_hash",
            ));
        }
    }
    Ok(())
}

/// Alias kept for docs / external naming symmetry with issue language.
pub type WorkflowRunRecord = WorkflowRun;
