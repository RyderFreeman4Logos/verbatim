//! Persistable stage records, hashes, fingerprints, costs, warnings and status.

use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use super::budget::{ComparisonBudget, ComparisonBudgetUsage};
use super::error::{ComparisonError, ComparisonResultType};
use super::stage::ComparisonStage;
use super::util::{require_digest, require_non_empty};

pub const COMPARE_SOURCES_WORKFLOW_SCHEMA_VERSION: u32 = 1;

/// Cost record for an individual stage, separately auditable from its artifact hash.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ComparisonCost {
    #[serde(default)]
    pub tokens: u64,
    #[serde(default)]
    pub cost_units: u64,
    #[serde(default)]
    pub wall_time_ms: u64,
}

/// Severity of a non-terminal comparison warning.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonWarningSeverity {
    Info,
    Warning,
}

/// Non-secret warning suitable for persistence with the run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonWarning {
    pub severity: ComparisonWarningSeverity,
    pub code: String,
    pub detail: String,
}

/// Terminal status. `Incomplete` includes typed fail-closed error causes in warnings/records.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonRunStatus {
    Running,
    Complete,
    Incomplete,
    Disabled,
}

impl ComparisonRunStatus {
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Incomplete | Self::Disabled)
    }
}

/// Immutable record of an attempted workflow stage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonStageRecord {
    pub stage: ComparisonStage,
    pub artifact_hash: Option<String>,
    pub input_fingerprint: Option<String>,
    pub output_fingerprint: Option<String>,
    pub usage_delta: ComparisonBudgetUsage,
    pub cost: ComparisonCost,
    pub ok: bool,
    pub detail: Option<String>,
}

impl ComparisonStageRecord {
    pub fn validate(&self) -> ComparisonResultType<()> {
        for (field, value) in [
            ("artifact_hash", &self.artifact_hash),
            ("input_fingerprint", &self.input_fingerprint),
            ("output_fingerprint", &self.output_fingerprint),
        ] {
            if let Some(value) = value {
                require_digest(field, value)?;
            }
        }
        if let Some(detail) = &self.detail {
            require_non_empty("stage_record.detail", detail)?;
        }
        Ok(())
    }
}

/// Persistence envelope for a two-sided comparison workflow.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareSourcesWorkflowRun {
    pub schema_version: u32,
    pub run_id: String,
    pub scope_hash: String,
    pub budget: ComparisonBudget,
    pub usage: ComparisonBudgetUsage,
    pub current_stage: ComparisonStage,
    pub records: Vec<ComparisonStageRecord>,
    pub warnings: Vec<ComparisonWarning>,
    pub status: ComparisonRunStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub comparison_result_hash: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_pack_hash: Option<String>,
}

/// Construction fields for [`CompareSourcesWorkflowRun`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompareSourcesWorkflowRunFields {
    pub run_id: String,
    pub scope_hash: String,
    pub budget: ComparisonBudget,
}

impl CompareSourcesWorkflowRun {
    pub fn new(fields: CompareSourcesWorkflowRunFields) -> ComparisonResultType<Self> {
        let run = Self {
            schema_version: COMPARE_SOURCES_WORKFLOW_SCHEMA_VERSION,
            run_id: fields.run_id,
            scope_hash: fields.scope_hash,
            budget: fields.budget,
            usage: ComparisonBudgetUsage::default(),
            current_stage: ComparisonStage::Decomposing,
            records: Vec::new(),
            warnings: Vec::new(),
            status: ComparisonRunStatus::Running,
            comparison_result_hash: None,
            context_pack_hash: None,
        };
        run.validate()?;
        Ok(run)
    }

    pub fn validate(&self) -> ComparisonResultType<()> {
        if self.schema_version != COMPARE_SOURCES_WORKFLOW_SCHEMA_VERSION {
            return Err(ComparisonError::validation(format!(
                "unsupported compare-sources schema version {}",
                self.schema_version
            )));
        }
        require_non_empty("run_id", &self.run_id)?;
        require_digest("scope_hash", &self.scope_hash)?;
        self.budget.validate()?;
        self.usage.check_against(&self.budget)?;
        for record in &self.records {
            record.validate()?;
        }
        for warning in &self.warnings {
            require_non_empty("warning.code", &warning.code)?;
            require_non_empty("warning.detail", &warning.detail)?;
        }
        if let Some(hash) = &self.comparison_result_hash {
            require_digest("comparison_result_hash", hash)?;
        }
        if let Some(hash) = &self.context_pack_hash {
            require_digest("context_pack_hash", hash)?;
        }
        if self.status == ComparisonRunStatus::Complete
            && (self.current_stage != ComparisonStage::Complete
                || self.comparison_result_hash.is_none()
                || self.context_pack_hash.is_none())
        {
            return Err(ComparisonError::validation(
                "complete run requires complete stage, comparison result hash, and context pack hash",
            ));
        }
        Ok(())
    }

    pub fn record_stage(&mut self, record: ComparisonStageRecord) -> ComparisonResultType<()> {
        record.validate()?;
        let cost_usage = ComparisonBudgetUsage {
            tokens: record.cost.tokens,
            cost_units: record.cost.cost_units,
            wall_time_ms: record.cost.wall_time_ms,
            ..ComparisonBudgetUsage::default()
        };
        let stage_usage = record.usage_delta.checked_add(&cost_usage, &self.budget)?;
        let candidate_usage = self.usage.checked_add(&stage_usage, &self.budget)?;
        self.usage = candidate_usage;
        self.current_stage = record.stage;
        self.records.push(record);
        self.validate()
    }

    pub fn add_warning(&mut self, warning: ComparisonWarning) -> ComparisonResultType<()> {
        require_non_empty("warning.code", &warning.code)?;
        require_non_empty("warning.detail", &warning.detail)?;
        self.warnings.push(warning);
        Ok(())
    }

    pub fn mark_incomplete(&mut self, detail: impl Into<String>) -> ComparisonResultType<()> {
        if self.status.is_terminal() {
            return Err(ComparisonError::IllegalTransition {
                detail: "cannot mark a terminal comparison run incomplete".into(),
            });
        }
        let detail = detail.into();
        require_non_empty("incomplete.detail", &detail)?;
        self.current_stage = ComparisonStage::Incomplete;
        self.status = ComparisonRunStatus::Incomplete;
        self.add_warning(ComparisonWarning {
            severity: ComparisonWarningSeverity::Warning,
            code: "incomplete".into(),
            detail,
        })?;
        self.validate()
    }

    pub fn mark_disabled(&mut self, detail: impl Into<String>) -> ComparisonResultType<()> {
        if self.status.is_terminal() {
            return Err(ComparisonError::IllegalTransition {
                detail: "cannot mark a terminal comparison run disabled".into(),
            });
        }
        let detail = detail.into();
        require_non_empty("disabled.detail", &detail)?;
        self.status = ComparisonRunStatus::Disabled;
        self.add_warning(ComparisonWarning {
            severity: ComparisonWarningSeverity::Info,
            code: "disabled".into(),
            detail,
        })?;
        self.validate()
    }

    pub fn complete(
        &mut self,
        result_hash: String,
        context_pack_hash: String,
    ) -> ComparisonResultType<()> {
        if self.status != ComparisonRunStatus::Running {
            return Err(ComparisonError::IllegalTransition {
                detail: "completion requires a running comparison run".into(),
            });
        }
        require_digest("comparison_result_hash", &result_hash)?;
        require_digest("context_pack_hash", &context_pack_hash)?;
        if self.current_stage != ComparisonStage::Rendering {
            return Err(ComparisonError::IllegalTransition {
                detail: "completion requires rendering stage".into(),
            });
        }
        self.comparison_result_hash = Some(result_hash);
        self.context_pack_hash = Some(context_pack_hash);
        self.current_stage = ComparisonStage::Complete;
        self.status = ComparisonRunStatus::Complete;
        self.validate()
    }
}

/// Stable SHA-256 content hash for a serializable comparison artifact.
pub fn content_hash_of<T: Serialize>(value: &T) -> ComparisonResultType<String> {
    let bytes = serde_json::to_vec(value)
        .map_err(|err| ComparisonError::validation(format!("hash encode: {err}")))?;
    let digest = Sha256::digest(bytes);
    Ok(format!("sha256:{digest:x}"))
}

pub fn encode_workflow_run_json(run: &CompareSourcesWorkflowRun) -> ComparisonResultType<Vec<u8>> {
    run.validate()?;
    serde_json::to_vec(run).map_err(|err| ComparisonError::validation(format!("run encode: {err}")))
}

pub fn decode_workflow_run_json(bytes: &[u8]) -> ComparisonResultType<CompareSourcesWorkflowRun> {
    let run: CompareSourcesWorkflowRun = decode_json(bytes, "run decode")?;
    run.validate()?;
    Ok(run)
}

fn decode_json<T: DeserializeOwned>(bytes: &[u8], context: &str) -> ComparisonResultType<T> {
    serde_json::from_slice(bytes)
        .map_err(|err| ComparisonError::validation(format!("{context}: {err}")))
}
