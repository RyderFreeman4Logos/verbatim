//! Typed failures for the exhaustive-audit contract.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

pub type ExhaustiveAuditResult<T> = Result<T, ExhaustiveAuditError>;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExhaustiveAuditBudgetDimension {
    ScopeMembers,
    Enumerations,
    Candidates,
    CostUnits,
    WallTimeMs,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExhaustiveAuditBudgetExhaustion {
    pub dimension: ExhaustiveAuditBudgetDimension,
    pub limit: u64,
    pub used: u64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum ExhaustiveAuditError {
    Validation {
        detail: String,
    },
    ScopeUnavailable {
        detail: String,
    },
    CoverageIncomplete {
        detail: String,
    },
    BudgetExhausted {
        exhaustion: ExhaustiveAuditBudgetExhaustion,
    },
    IllegalTransition {
        detail: String,
    },
    Disabled {
        detail: String,
    },
}

impl ExhaustiveAuditError {
    pub fn validation(detail: impl Into<String>) -> Self {
        Self::Validation {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for ExhaustiveAuditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation { detail } => write!(f, "exhaustive audit validation: {detail}"),
            Self::ScopeUnavailable { detail } => write!(f, "audit scope unavailable: {detail}"),
            Self::CoverageIncomplete { detail } => write!(f, "audit coverage incomplete: {detail}"),
            Self::BudgetExhausted { exhaustion } => write!(
                f,
                "audit budget exhausted: {:?} used {} limit {}",
                exhaustion.dimension, exhaustion.used, exhaustion.limit
            ),
            Self::IllegalTransition { detail } => write!(f, "illegal audit transition: {detail}"),
            Self::Disabled { detail } => write!(f, "exhaustive audit disabled: {detail}"),
        }
    }
}

impl Error for ExhaustiveAuditError {}
