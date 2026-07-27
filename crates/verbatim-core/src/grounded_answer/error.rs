//! Typed fail-closed errors for the grounded-answer workflow contract.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::stage::WorkflowStage;

/// Result alias for workflow contract operations.
pub type WorkflowResult<T> = Result<T, WorkflowError>;

/// Typed workflow failures. Adapters must project model/transport faults into
/// these classes rather than inventing a verified answer.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum WorkflowError {
    /// Structural or semantic validation of a contract artifact failed.
    Validation { detail: String },
    /// Requested stage transition is illegal for the current state machine.
    IllegalTransition {
        from: WorkflowStage,
        to: WorkflowStage,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Policy gate refused the operation (intent/risk/budget/privacy).
    PolicyDenied {
        gate: String,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Model output failed schema/claim/citation verification.
    VerificationFailed {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Model or generation path failed (timeout, malformed, unavailable).
    ModelFailure {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Required evidence or identity is missing / unresolvable.
    MissingEvidence {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Budget (tokens/cost/latency/revisions) exhausted.
    BudgetExhausted {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Workflow is disabled or capability unavailable; R/RA must still work.
    Disabled {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

impl WorkflowError {
    pub fn validation(detail: impl Into<String>) -> Self {
        Self::Validation {
            detail: detail.into(),
        }
    }

    pub fn illegal_transition(
        from: WorkflowStage,
        to: WorkflowStage,
        detail: impl Into<String>,
    ) -> Self {
        Self::IllegalTransition {
            from,
            to,
            detail: Some(detail.into()),
        }
    }

    pub fn policy_denied(gate: impl Into<String>, detail: impl Into<String>) -> Self {
        Self::PolicyDenied {
            gate: gate.into(),
            detail: Some(detail.into()),
        }
    }

    pub fn verification_failed(detail: impl Into<String>) -> Self {
        Self::VerificationFailed {
            detail: Some(detail.into()),
        }
    }

    pub fn model_failure(detail: impl Into<String>) -> Self {
        Self::ModelFailure {
            detail: Some(detail.into()),
        }
    }

    pub fn missing_evidence(detail: impl Into<String>) -> Self {
        Self::MissingEvidence {
            detail: Some(detail.into()),
        }
    }

    pub fn budget_exhausted(detail: impl Into<String>) -> Self {
        Self::BudgetExhausted {
            detail: Some(detail.into()),
        }
    }

    pub fn disabled(detail: impl Into<String>) -> Self {
        Self::Disabled {
            detail: Some(detail.into()),
        }
    }

    /// Stable class name for logs / metrics (not user-facing prose).
    pub fn class_name(&self) -> &'static str {
        match self {
            Self::Validation { .. } => "validation",
            Self::IllegalTransition { .. } => "illegal_transition",
            Self::PolicyDenied { .. } => "policy_denied",
            Self::VerificationFailed { .. } => "verification_failed",
            Self::ModelFailure { .. } => "model_failure",
            Self::MissingEvidence { .. } => "missing_evidence",
            Self::BudgetExhausted { .. } => "budget_exhausted",
            Self::Disabled { .. } => "disabled",
        }
    }

    /// Whether this failure class must degrade to typed abstention (never a
    /// published verified answer).
    pub fn requires_abstention(&self) -> bool {
        !matches!(self, Self::Disabled { .. })
    }
}

impl fmt::Display for WorkflowError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation { detail } => write!(f, "workflow validation: {detail}"),
            Self::IllegalTransition { from, to, detail } => {
                write!(f, "illegal workflow transition {from} -> {to}")?;
                if let Some(d) = detail {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
            Self::PolicyDenied { gate, detail } => {
                write!(f, "workflow policy denied at gate {gate}")?;
                if let Some(d) = detail {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
            Self::VerificationFailed { detail } => {
                write!(f, "workflow verification failed")?;
                if let Some(d) = detail {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
            Self::ModelFailure { detail } => {
                write!(f, "workflow model failure")?;
                if let Some(d) = detail {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
            Self::MissingEvidence { detail } => {
                write!(f, "workflow missing evidence")?;
                if let Some(d) = detail {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
            Self::BudgetExhausted { detail } => {
                write!(f, "workflow budget exhausted")?;
                if let Some(d) = detail {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
            Self::Disabled { detail } => {
                write!(f, "grounded-answer workflow disabled")?;
                if let Some(d) = detail {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
        }
    }
}

impl Error for WorkflowError {}
