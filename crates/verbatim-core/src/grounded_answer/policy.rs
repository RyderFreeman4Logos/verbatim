//! Policy gate types for the grounded-answer workflow (trait + types only).

use serde::{Deserialize, Serialize};

use super::claim::require_non_empty;
use super::error::{WorkflowError, WorkflowResult};
use super::stage::WorkflowStage;

/// Policy gate categories applied before model-touching stages.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyGateKind {
    Intent,
    Risk,
    Completeness,
    Privacy,
    Budget,
    /// Catch-all for residual policy domains.
    Other,
}

impl PolicyGateKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Intent => "intent",
            Self::Risk => "risk",
            Self::Completeness => "completeness",
            Self::Privacy => "privacy",
            Self::Budget => "budget",
            Self::Other => "other",
        }
    }
}

/// Named gate instance evaluated at a stage boundary.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyGate {
    pub kind: PolicyGateKind,
    pub name: String,
    /// Stage at which this gate must be evaluated.
    pub stage: WorkflowStage,
}

impl PolicyGate {
    pub fn new(
        kind: PolicyGateKind,
        name: impl Into<String>,
        stage: WorkflowStage,
    ) -> WorkflowResult<Self> {
        let gate = Self {
            kind,
            name: name.into(),
            stage,
        };
        gate.validate()?;
        Ok(gate)
    }

    pub fn validate(&self) -> WorkflowResult<()> {
        require_non_empty("policy_gate.name", &self.name)
    }
}

/// Outcome of a policy evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PolicyDecisionKind {
    Allow,
    Deny,
    /// Allow with a recorded warning (still fail-closed on later verify).
    AllowWithWarning,
}

impl PolicyDecisionKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Allow => "allow",
            Self::Deny => "deny",
            Self::AllowWithWarning => "allow_with_warning",
        }
    }

    pub fn is_permitted(self) -> bool {
        matches!(self, Self::Allow | Self::AllowWithWarning)
    }
}

/// Concrete decision for one gate evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PolicyDecision {
    pub gate: PolicyGate,
    pub decision: PolicyDecisionKind,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reason: Option<String>,
}

impl PolicyDecision {
    pub fn allow(gate: PolicyGate) -> Self {
        Self {
            gate,
            decision: PolicyDecisionKind::Allow,
            reason: None,
        }
    }

    pub fn deny(gate: PolicyGate, reason: impl Into<String>) -> Self {
        Self {
            gate,
            decision: PolicyDecisionKind::Deny,
            reason: Some(reason.into()),
        }
    }

    pub fn validate(&self) -> WorkflowResult<()> {
        self.gate.validate()?;
        if let Some(r) = &self.reason {
            require_non_empty("policy_decision.reason", r)?;
        }
        if self.decision == PolicyDecisionKind::Deny && self.reason.is_none() {
            return Err(WorkflowError::validation(
                "deny policy decision requires a reason",
            ));
        }
        Ok(())
    }

    pub fn into_result(self) -> WorkflowResult<Self> {
        self.validate()?;
        if self.decision.is_permitted() {
            Ok(self)
        } else {
            Err(WorkflowError::policy_denied(
                self.gate.name.clone(),
                self.reason.unwrap_or_else(|| "denied".into()),
            ))
        }
    }
}

/// Minimal policy context supplied to gates (no Store types).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct WorkflowPolicyContext {
    pub principal: String,
    pub profile_ref: String,
    pub policy_version: String,
    /// Whether a text model endpoint is configured (workflow may be disabled).
    pub model_enabled: bool,
    /// Remaining revision budget for generate→verify loops.
    pub remaining_revisions: u32,
    /// Optional max cost units remaining (opaque integer budget).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub remaining_cost_units: Option<u64>,
}

/// Field bundle for [`WorkflowPolicyContext::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct WorkflowPolicyContextFields {
    pub principal: String,
    pub profile_ref: String,
    pub policy_version: String,
    pub model_enabled: bool,
    pub remaining_revisions: u32,
    pub remaining_cost_units: Option<u64>,
}

impl WorkflowPolicyContext {
    pub fn new(fields: WorkflowPolicyContextFields) -> WorkflowResult<Self> {
        let ctx = Self {
            principal: fields.principal,
            profile_ref: fields.profile_ref,
            policy_version: fields.policy_version,
            model_enabled: fields.model_enabled,
            remaining_revisions: fields.remaining_revisions,
            remaining_cost_units: fields.remaining_cost_units,
        };
        ctx.validate()?;
        Ok(ctx)
    }

    pub fn validate(&self) -> WorkflowResult<()> {
        require_non_empty("principal", &self.principal)?;
        require_non_empty("profile_ref", &self.profile_ref)?;
        require_non_empty("policy_version", &self.policy_version)?;
        Ok(())
    }

    /// Fail closed when the model path is disabled (R/RA still available).
    pub fn require_model_enabled(&self) -> WorkflowResult<()> {
        if self.model_enabled {
            Ok(())
        } else {
            Err(WorkflowError::disabled(
                "no text-model endpoint; grounded-answer workflow unavailable",
            ))
        }
    }
}
