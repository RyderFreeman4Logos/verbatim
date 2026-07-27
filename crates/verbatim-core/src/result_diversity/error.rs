//! Typed, diagnostic-only errors for result-diversity contracts.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::{DiversityBudgetExhaustion, DiversityStage};

pub type DiversityResult<T> = Result<T, DiversityError>;

/// Errors intentionally retain only contract diagnostics, never document text,
/// embeddings, locators, credentials, or other secret-bearing values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum DiversityError {
    Validation {
        detail: String,
    },
    BudgetExhausted {
        exhaustion: DiversityBudgetExhaustion,
    },
    IllegalTransition {
        from: DiversityStage,
        to: DiversityStage,
    },
    Disabled {
        detail: String,
    },
}

impl DiversityError {
    pub fn validation(detail: impl Into<String>) -> Self {
        Self::Validation {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for DiversityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation { detail } => write!(f, "result-diversity validation: {detail}"),
            Self::BudgetExhausted { exhaustion } => write!(
                f,
                "result-diversity budget exhausted: {} used {} limit {}",
                exhaustion.dimension.as_str(),
                exhaustion.used,
                exhaustion.limit
            ),
            Self::IllegalTransition { from, to } => {
                write!(f, "illegal result-diversity transition {from} -> {to}")
            }
            Self::Disabled { detail } => write!(f, "result-diversity disabled: {detail}"),
        }
    }
}

impl Error for DiversityError {}
