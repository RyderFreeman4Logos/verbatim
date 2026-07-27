//! WorkflowRun persistence envelope for multi-hop research.

use serde::{Deserialize, Serialize};

use super::budget::{ResearchBudget, ResearchBudgetUsage};
use super::error::{ResearchError, ResearchResult};
use super::merge::MergedContextPack;
use super::stage::ResearchRound;
use super::util::{require_digest, require_non_empty};
use crate::wire_schemas::{encode_wire_document, wire_content_hash};

/// Schema version for multi-hop research WorkflowRun documents.
pub const MULTI_HOP_RESEARCH_WORKFLOW_SCHEMA_VERSION: u32 = 1;

/// Final status of a multi-hop research run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchFinalStatus {
    /// Coverage complete; merged pack available.
    Complete,
    /// Incomplete / ambiguous / budget exhausted / fail-closed.
    Incomplete,
    /// Workflow disabled.
    Disabled,
    /// Still running (not terminal).
    InProgress,
}

impl ResearchFinalStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
            Self::Disabled => "disabled",
            Self::InProgress => "in_progress",
        }
    }

    pub fn is_terminal(self) -> bool {
        !matches!(self, Self::InProgress)
    }
}

/// Severity for non-fatal research warnings.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchWarningSeverity {
    Info,
    Warning,
    Error,
}

impl ResearchWarningSeverity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Info => "info",
            Self::Warning => "warning",
            Self::Error => "error",
        }
    }
}

/// Non-fatal warning recorded on a research run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchWarning {
    pub severity: ResearchWarningSeverity,
    pub code: String,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub round: Option<ResearchRound>,
}

impl ResearchWarning {
    pub fn new(
        severity: ResearchWarningSeverity,
        code: impl Into<String>,
        message: impl Into<String>,
    ) -> ResearchResult<Self> {
        let w = Self {
            severity,
            code: code.into(),
            message: message.into(),
            round: None,
        };
        w.validate()?;
        Ok(w)
    }

    pub fn validate(&self) -> ResearchResult<()> {
        require_non_empty("warning.code", &self.code)?;
        require_non_empty("warning.message", &self.message)?;
        Ok(())
    }
}

/// Per-round record inside a multi-hop research WorkflowRun.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchRoundRecord {
    pub round: ResearchRound,
    /// 1-based round index for retrieving/corrective rounds; 0 for decompose-only.
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub round_index: u32,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub artifact_hash: Option<String>,
    #[serde(default)]
    pub usage_delta: ResearchBudgetUsage,
    pub ok: bool,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

impl ResearchRoundRecord {
    pub fn validate(&self) -> ResearchResult<()> {
        if let Some(h) = &self.artifact_hash {
            require_digest("round.artifact_hash", h)?;
        }
        if let Some(d) = &self.detail {
            require_non_empty("round.detail", d)?;
        }
        Ok(())
    }
}

/// Persistence envelope for one multi-hop research workflow execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowRun {
    pub schema_version: u32,
    pub run_id: String,
    pub research_question_hash: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decomposition_plan_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub coverage_report_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub merged_context_pack_hash: Option<String>,
    pub current_round: ResearchRound,
    pub final_status: ResearchFinalStatus,
    pub budget: ResearchBudget,
    #[serde(default)]
    pub usage: ResearchBudgetUsage,
    pub rounds: Vec<ResearchRoundRecord>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<ResearchWarning>,
    /// Required when status is Incomplete.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub incomplete_reason: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_ref: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
}

/// Field bundle for [`WorkflowRun::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowRunFields {
    pub run_id: String,
    pub research_question_hash: String,
    pub budget: ResearchBudget,
    pub profile_ref: Option<String>,
    pub generation: Option<String>,
}

impl WorkflowRun {
    /// Start a new in-progress run bound to a research question hash.
    pub fn new(fields: WorkflowRunFields) -> ResearchResult<Self> {
        fields.budget.validate()?;
        let run = Self {
            schema_version: MULTI_HOP_RESEARCH_WORKFLOW_SCHEMA_VERSION,
            run_id: fields.run_id,
            research_question_hash: fields.research_question_hash,
            decomposition_plan_hash: None,
            coverage_report_hash: None,
            merged_context_pack_hash: None,
            current_round: ResearchRound::Decomposing,
            final_status: ResearchFinalStatus::InProgress,
            budget: fields.budget,
            usage: ResearchBudgetUsage::default(),
            rounds: vec![ResearchRoundRecord {
                round: ResearchRound::Decomposing,
                round_index: 0,
                artifact_hash: None,
                usage_delta: ResearchBudgetUsage::default(),
                ok: true,
                detail: None,
            }],
            warnings: vec![],
            incomplete_reason: None,
            profile_ref: fields.profile_ref,
            generation: fields.generation,
        };
        run.validate()?;
        Ok(run)
    }

    pub fn validate(&self) -> ResearchResult<()> {
        if self.schema_version != MULTI_HOP_RESEARCH_WORKFLOW_SCHEMA_VERSION {
            return Err(ResearchError::validation(format!(
                "unknown multi-hop research schema_version {}",
                self.schema_version
            )));
        }
        require_non_empty("run_id", &self.run_id)?;
        require_digest("research_question_hash", &self.research_question_hash)?;
        if let Some(h) = &self.decomposition_plan_hash {
            require_digest("decomposition_plan_hash", h)?;
        }
        if let Some(h) = &self.coverage_report_hash {
            require_digest("coverage_report_hash", h)?;
        }
        if let Some(h) = &self.merged_context_pack_hash {
            require_digest("merged_context_pack_hash", h)?;
        }
        self.budget.validate()?;
        self.usage.check_against(&self.budget).or_else(|err| {
            // Terminal incomplete may legitimately record exhaustion; still
            // allow validate if final_status is Incomplete.
            if self.final_status == ResearchFinalStatus::Incomplete {
                Ok(())
            } else {
                Err(err)
            }
        })?;
        if self.rounds.is_empty() {
            return Err(ResearchError::validation(
                "workflow run requires at least one round record",
            ));
        }
        for r in &self.rounds {
            r.validate()?;
        }
        for w in &self.warnings {
            w.validate()?;
        }
        if let Some(p) = &self.profile_ref {
            require_non_empty("profile_ref", p)?;
        }
        if let Some(g) = &self.generation {
            require_non_empty("generation", g)?;
        }
        match self.final_status {
            ResearchFinalStatus::Complete => {
                if self.current_round != ResearchRound::Complete {
                    return Err(ResearchError::validation(
                        "complete status requires complete round",
                    ));
                }
                if self.merged_context_pack_hash.is_none() {
                    return Err(ResearchError::validation(
                        "complete status requires merged_context_pack_hash",
                    ));
                }
            }
            ResearchFinalStatus::Incomplete => {
                if self.current_round != ResearchRound::Incomplete {
                    return Err(ResearchError::validation(
                        "incomplete status requires incomplete round",
                    ));
                }
                match &self.incomplete_reason {
                    Some(r) => require_non_empty("incomplete_reason", r)?,
                    None => {
                        return Err(ResearchError::validation(
                            "incomplete status requires incomplete_reason",
                        ));
                    }
                }
            }
            ResearchFinalStatus::Disabled => {
                if self.current_round != ResearchRound::Incomplete {
                    return Err(ResearchError::validation(
                        "disabled status requires incomplete round",
                    ));
                }
            }
            ResearchFinalStatus::InProgress => {
                if self.current_round.is_terminal() {
                    return Err(ResearchError::validation(
                        "in_progress status cannot use terminal round",
                    ));
                }
            }
        }
        Ok(())
    }

    /// Record a successful intermediate round transition (non-terminal).
    pub fn record_round(&mut self, record: ResearchRoundRecord) -> ResearchResult<()> {
        if self.final_status.is_terminal() {
            return Err(ResearchError::validation(
                "cannot record round on terminal workflow run",
            ));
        }
        record.validate()?;
        if !is_legal_round_record(self.current_round, record.round) {
            return Err(ResearchError::illegal_transition(
                self.current_round,
                record.round,
                "illegal record_round successor",
            ));
        }
        let projected = self.usage.saturating_add(&record.usage_delta);
        projected.check_against(&self.budget)?;
        self.usage = projected;
        if let Some(h) = &record.artifact_hash {
            match record.round {
                ResearchRound::Decomposing => {
                    self.decomposition_plan_hash = Some(h.clone());
                }
                ResearchRound::EvaluatingCoverage => {
                    self.coverage_report_hash = Some(h.clone());
                }
                ResearchRound::Retrieving | ResearchRound::CorrectiveRound => {}
                ResearchRound::Complete | ResearchRound::Incomplete => {
                    return Err(ResearchError::validation(
                        "use complete()/mark_incomplete() for terminal rounds",
                    ));
                }
            }
        }
        self.current_round = record.round;
        self.rounds.push(record);
        self.validate()
    }

    /// Mark the run complete with a merged context pack.
    pub fn complete(&mut self, pack: &MergedContextPack) -> ResearchResult<()> {
        pack.validate()?;
        if self.final_status.is_terminal() {
            return Err(ResearchError::validation(
                "cannot complete an already terminal run",
            ));
        }
        if !matches!(
            self.current_round,
            ResearchRound::EvaluatingCoverage | ResearchRound::CorrectiveRound
        ) {
            return Err(ResearchError::illegal_transition(
                self.current_round,
                ResearchRound::Complete,
                "complete requires evaluating_coverage or corrective_round",
            ));
        }
        if pack.research_question_hash != self.research_question_hash {
            return Err(ResearchError::validation(
                "merged pack research_question_hash does not match run",
            ));
        }
        if let Some(plan_hash) = &self.decomposition_plan_hash {
            if plan_hash != &pack.decomposition_plan_hash {
                return Err(ResearchError::validation(
                    "merged pack decomposition_plan_hash does not match run",
                ));
            }
        }
        let hash = content_hash_of(pack)?;
        self.merged_context_pack_hash = Some(hash.clone());
        self.current_round = ResearchRound::Complete;
        self.final_status = ResearchFinalStatus::Complete;
        self.rounds.push(ResearchRoundRecord {
            round: ResearchRound::Complete,
            round_index: 0,
            artifact_hash: Some(hash),
            usage_delta: ResearchBudgetUsage::default(),
            ok: true,
            detail: None,
        });
        self.validate()
    }

    /// Mark the run incomplete (coverage gap, budget, injection, model failure).
    pub fn mark_incomplete(&mut self, reason: impl Into<String>) -> ResearchResult<()> {
        if self.final_status.is_terminal() {
            return Err(ResearchError::validation(
                "cannot mark incomplete on terminal run",
            ));
        }
        let reason = reason.into();
        require_non_empty("incomplete_reason", &reason)?;
        self.current_round = ResearchRound::Incomplete;
        self.final_status = ResearchFinalStatus::Incomplete;
        self.incomplete_reason = Some(reason.clone());
        self.rounds.push(ResearchRoundRecord {
            round: ResearchRound::Incomplete,
            round_index: 0,
            artifact_hash: None,
            usage_delta: ResearchBudgetUsage::default(),
            ok: false,
            detail: Some(reason),
        });
        self.validate()
    }

    /// Mark the workflow disabled (R/RA path remains available).
    pub fn mark_disabled(&mut self, reason: impl Into<String>) -> ResearchResult<()> {
        if self.final_status.is_terminal() {
            return Err(ResearchError::validation(
                "cannot mark disabled on terminal run",
            ));
        }
        let reason = reason.into();
        require_non_empty("incomplete_reason", &reason)?;
        self.current_round = ResearchRound::Incomplete;
        self.final_status = ResearchFinalStatus::Disabled;
        self.incomplete_reason = Some(reason.clone());
        self.rounds.push(ResearchRoundRecord {
            round: ResearchRound::Incomplete,
            round_index: 0,
            artifact_hash: None,
            usage_delta: ResearchBudgetUsage::default(),
            ok: false,
            detail: Some(reason),
        });
        self.validate()
    }

    pub fn content_hash(&self) -> ResearchResult<String> {
        content_hash_of(self)
    }
}

fn is_legal_round_record(current: ResearchRound, next: ResearchRound) -> bool {
    if next == ResearchRound::Incomplete {
        return !current.is_terminal();
    }
    matches!(
        (current, next),
        (ResearchRound::Decomposing, ResearchRound::Retrieving)
            | (ResearchRound::Retrieving, ResearchRound::EvaluatingCoverage)
            | (
                ResearchRound::EvaluatingCoverage,
                ResearchRound::CorrectiveRound
            )
            | (ResearchRound::CorrectiveRound, ResearchRound::Retrieving)
            | (
                ResearchRound::CorrectiveRound,
                ResearchRound::EvaluatingCoverage
            )
    )
}

/// Decode a multi-hop WorkflowRun from JSON and fail closed on unknown schema.
pub fn decode_workflow_run_json(bytes: &[u8]) -> ResearchResult<WorkflowRun> {
    let value: WorkflowRun = serde_json::from_slice(bytes)
        .map_err(|err| ResearchError::validation(format!("workflow run JSON decode: {err}")))?;
    value.validate()?;
    Ok(value)
}

/// Encode a multi-hop WorkflowRun to compact JSON bytes.
pub fn encode_workflow_run_json(run: &WorkflowRun) -> ResearchResult<Vec<u8>> {
    run.validate()?;
    encode_wire_document(run)
        .map_err(|err| ResearchError::validation(format!("workflow run JSON encode: {err}")))
}

/// Stable content hash of a serializable research artifact.
pub fn content_hash_of<T: Serialize>(value: &T) -> ResearchResult<String> {
    let bytes = encode_wire_document(value)
        .map_err(|err| ResearchError::validation(format!("content hash encode: {err}")))?;
    Ok(wire_content_hash(&bytes))
}

/// Alias for docs / external naming symmetry.
pub type WorkflowRunRecord = WorkflowRun;
