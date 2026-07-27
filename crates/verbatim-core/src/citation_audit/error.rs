//! Typed, fail-closed errors for citation audits.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::{CitationAuditBudgetExhaustion, CitationAuditStage};

pub type CitationAuditResult<T> = Result<T, CitationAuditError>;

/// Failure classes emitted by the pure citation-audit contract.
///
/// Details are contract diagnostics only; implementations must not put source
/// document, quote, locator, or credential text into them.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum CitationAuditError {
    Validation {
        detail: String,
    },
    EvidenceRejected {
        detail: String,
    },
    UntrustedControl {
        detail: String,
    },
    BudgetExhausted {
        exhaustion: CitationAuditBudgetExhaustion,
    },
    IllegalTransition {
        from: CitationAuditStage,
        to: CitationAuditStage,
    },
    ModelFailure {
        detail: String,
    },
    Disabled {
        detail: String,
    },
}

impl CitationAuditError {
    pub fn validation(detail: impl Into<String>) -> Self {
        Self::Validation {
            detail: detail.into(),
        }
    }

    pub fn evidence_rejected(detail: impl Into<String>) -> Self {
        Self::EvidenceRejected {
            detail: detail.into(),
        }
    }
}

impl fmt::Display for CitationAuditError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation { detail } => write!(f, "citation-audit validation: {detail}"),
            Self::EvidenceRejected { detail } => {
                write!(f, "citation-audit evidence rejected: {detail}")
            }
            Self::UntrustedControl { detail } => {
                write!(f, "citation-audit untrusted control: {detail}")
            }
            Self::BudgetExhausted { exhaustion } => write!(
                f,
                "citation-audit budget exhausted: {} used {} limit {}",
                exhaustion.dimension.as_str(),
                exhaustion.used,
                exhaustion.limit
            ),
            Self::IllegalTransition { from, to } => {
                write!(f, "illegal citation-audit transition {from} -> {to}")
            }
            Self::ModelFailure { detail } => write!(f, "citation-audit model failure: {detail}"),
            Self::Disabled { detail } => write!(f, "citation-audit disabled: {detail}"),
        }
    }
}

impl Error for CitationAuditError {}
