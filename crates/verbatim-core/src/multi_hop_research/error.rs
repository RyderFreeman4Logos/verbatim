//! Typed fail-closed errors for the multi-hop research workflow contract.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::stage::ResearchRound;

/// Result alias for multi-hop research contract operations.
pub type ResearchResult<T> = Result<T, ResearchError>;

/// Which budget dimension was exhausted (for typed fail-closed errors).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetDimension {
    Rounds,
    Subqueries,
    Candidates,
    Tokens,
    EndpointCalls,
    CostUnits,
    WallTimeMs,
}

impl BudgetDimension {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Rounds => "rounds",
            Self::Subqueries => "subqueries",
            Self::Candidates => "candidates",
            Self::Tokens => "tokens",
            Self::EndpointCalls => "endpoint_calls",
            Self::CostUnits => "cost_units",
            Self::WallTimeMs => "wall_time_ms",
        }
    }
}

/// Structured budget exhaustion detail (limit + observed usage).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetExhaustion {
    pub dimension: BudgetDimension,
    pub limit: u64,
    pub used: u64,
}

impl BudgetExhaustion {
    pub fn new(dimension: BudgetDimension, limit: u64, used: u64) -> Self {
        Self {
            dimension,
            limit,
            used,
        }
    }

    pub fn validate(&self) -> ResearchResult<()> {
        if self.used <= self.limit {
            return Err(ResearchError::validation(
                "budget exhaustion requires used > limit",
            ));
        }
        Ok(())
    }
}

/// Typed multi-hop research failures. Adapters must project transport/model
/// faults into these classes rather than inventing complete coverage.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum ResearchError {
    /// Structural or semantic validation of a contract artifact failed.
    Validation { detail: String },
    /// Requested round transition is illegal for the current state machine.
    IllegalTransition {
        from: ResearchRound,
        to: ResearchRound,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Declared budget dimension exhausted (fail closed, never open-ended).
    BudgetExhausted {
        exhaustion: BudgetExhaustion,
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Coverage is incomplete / ambiguous; overclaiming is forbidden.
    IncompleteCoverage {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Evidence or document text attempted to alter workflow instructions.
    InjectionRejected {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Required evidence path / dependency is missing or unresolvable.
    MissingEvidence {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Model / decomposition / retrieval adapter failed.
    ModelFailure {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
    /// Workflow is disabled or capability unavailable.
    Disabled {
        #[serde(default, skip_serializing_if = "Option::is_none")]
        detail: Option<String>,
    },
}

impl ResearchError {
    pub fn validation(detail: impl Into<String>) -> Self {
        Self::Validation {
            detail: detail.into(),
        }
    }

    pub fn illegal_transition(
        from: ResearchRound,
        to: ResearchRound,
        detail: impl Into<String>,
    ) -> Self {
        Self::IllegalTransition {
            from,
            to,
            detail: Some(detail.into()),
        }
    }

    pub fn budget_exhausted(exhaustion: BudgetExhaustion, detail: impl Into<String>) -> Self {
        Self::BudgetExhausted {
            exhaustion,
            detail: Some(detail.into()),
        }
    }

    pub fn incomplete_coverage(detail: impl Into<String>) -> Self {
        Self::IncompleteCoverage {
            detail: Some(detail.into()),
        }
    }

    pub fn injection_rejected(detail: impl Into<String>) -> Self {
        Self::InjectionRejected {
            detail: Some(detail.into()),
        }
    }

    pub fn missing_evidence(detail: impl Into<String>) -> Self {
        Self::MissingEvidence {
            detail: Some(detail.into()),
        }
    }

    pub fn model_failure(detail: impl Into<String>) -> Self {
        Self::ModelFailure {
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
            Self::BudgetExhausted { .. } => "budget_exhausted",
            Self::IncompleteCoverage { .. } => "incomplete_coverage",
            Self::InjectionRejected { .. } => "injection_rejected",
            Self::MissingEvidence { .. } => "missing_evidence",
            Self::ModelFailure { .. } => "model_failure",
            Self::Disabled { .. } => "disabled",
        }
    }

    /// Whether this failure class must terminate without claiming complete coverage.
    pub fn requires_incomplete(&self) -> bool {
        !matches!(self, Self::Disabled { .. })
    }
}

impl fmt::Display for ResearchError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation { detail } => write!(f, "research validation: {detail}"),
            Self::IllegalTransition { from, to, detail } => {
                write!(f, "illegal research transition {from} -> {to}")?;
                if let Some(d) = detail {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
            Self::BudgetExhausted { exhaustion, detail } => {
                write!(
                    f,
                    "research budget exhausted ({} used {} limit {})",
                    exhaustion.dimension.as_str(),
                    exhaustion.used,
                    exhaustion.limit
                )?;
                if let Some(d) = detail {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
            Self::IncompleteCoverage { detail } => {
                write!(f, "research coverage incomplete")?;
                if let Some(d) = detail {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
            Self::InjectionRejected { detail } => {
                write!(f, "research injection rejected")?;
                if let Some(d) = detail {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
            Self::MissingEvidence { detail } => {
                write!(f, "research missing evidence")?;
                if let Some(d) = detail {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
            Self::ModelFailure { detail } => {
                write!(f, "research model failure")?;
                if let Some(d) = detail {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
            Self::Disabled { detail } => {
                write!(f, "multi-hop research workflow disabled")?;
                if let Some(d) = detail {
                    write!(f, ": {d}")?;
                }
                Ok(())
            }
        }
    }
}

impl Error for ResearchError {}
