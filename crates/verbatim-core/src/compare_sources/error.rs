//! Typed errors for the fail-closed comparison contract.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Result alias for compare-sources contract operations.
pub type ComparisonResultType<T> = Result<T, ComparisonError>;

/// Budget dimension that exceeded its declared cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonBudgetDimension {
    Dimensions,
    Sources,
    Candidates,
    Tokens,
    CostUnits,
    WallTimeMs,
}

impl ComparisonBudgetDimension {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Dimensions => "dimensions",
            Self::Sources => "sources",
            Self::Candidates => "candidates",
            Self::Tokens => "tokens",
            Self::CostUnits => "cost_units",
            Self::WallTimeMs => "wall_time_ms",
        }
    }
}

/// Structured evidence that a hard comparison budget was exceeded.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonBudgetExhaustion {
    pub dimension: ComparisonBudgetDimension,
    pub limit: u64,
    pub used: u64,
}

impl ComparisonBudgetExhaustion {
    pub fn new(dimension: ComparisonBudgetDimension, limit: u64, used: u64) -> Self {
        Self {
            dimension,
            limit,
            used,
        }
    }

    pub fn validate(&self) -> ComparisonResultType<()> {
        if self.used <= self.limit {
            return Err(ComparisonError::validation(
                "budget exhaustion requires used > limit",
            ));
        }
        Ok(())
    }
}

/// Typed failures for source/version comparison adapters.
///
/// Adapters must return these errors rather than silently dropping an
/// unauthorized version, unavailable version, missing citation, or budget cap.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum ComparisonError {
    Validation {
        detail: String,
    },
    ScopeUnresolved {
        detail: Option<String>,
    },
    VersionGone {
        source_id: String,
        version_id: String,
    },
    AclDenied {
        source_id: String,
        version_id: String,
    },
    BudgetExhausted {
        exhaustion: ComparisonBudgetExhaustion,
        detail: Option<String>,
    },
    MissingEvidence {
        detail: Option<String>,
    },
    IllegalTransition {
        detail: String,
    },
    Disabled {
        detail: Option<String>,
    },
}

impl ComparisonError {
    pub fn validation(detail: impl Into<String>) -> Self {
        Self::Validation {
            detail: detail.into(),
        }
    }

    pub fn scope_unresolved(detail: impl Into<String>) -> Self {
        Self::ScopeUnresolved {
            detail: Some(detail.into()),
        }
    }

    pub fn budget_exhausted(
        exhaustion: ComparisonBudgetExhaustion,
        detail: impl Into<String>,
    ) -> Self {
        Self::BudgetExhausted {
            exhaustion,
            detail: Some(detail.into()),
        }
    }

    pub fn missing_evidence(detail: impl Into<String>) -> Self {
        Self::MissingEvidence {
            detail: Some(detail.into()),
        }
    }

    pub fn disabled(detail: impl Into<String>) -> Self {
        Self::Disabled {
            detail: Some(detail.into()),
        }
    }

    pub fn class_name(&self) -> &'static str {
        match self {
            Self::Validation { .. } => "validation",
            Self::ScopeUnresolved { .. } => "scope_unresolved",
            Self::VersionGone { .. } => "version_gone",
            Self::AclDenied { .. } => "acl_denied",
            Self::BudgetExhausted { .. } => "budget_exhausted",
            Self::MissingEvidence { .. } => "missing_evidence",
            Self::IllegalTransition { .. } => "illegal_transition",
            Self::Disabled { .. } => "disabled",
        }
    }

    /// Except for an explicit disabled capability, errors terminate a run as
    /// incomplete rather than allowing a plausible partial comparison.
    pub fn requires_incomplete(&self) -> bool {
        !matches!(self, Self::Disabled { .. })
    }
}

impl fmt::Display for ComparisonError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Validation { detail } => write!(f, "comparison validation: {detail}"),
            Self::ScopeUnresolved { detail } => {
                write_optional(f, "comparison scope unresolved", detail)
            }
            Self::VersionGone {
                source_id,
                version_id,
            } => {
                write!(f, "comparison version gone: {source_id}@{version_id}")
            }
            Self::AclDenied {
                source_id,
                version_id,
            } => {
                write!(f, "comparison ACL denied: {source_id}@{version_id}")
            }
            Self::BudgetExhausted { exhaustion, detail } => write_optional(
                f,
                &format!(
                    "comparison budget exhausted ({} used {} limit {})",
                    exhaustion.dimension.as_str(),
                    exhaustion.used,
                    exhaustion.limit
                ),
                detail,
            ),
            Self::MissingEvidence { detail } => {
                write_optional(f, "comparison missing evidence", detail)
            }
            Self::IllegalTransition { detail } => {
                write!(f, "illegal comparison transition: {detail}")
            }
            Self::Disabled { detail } => {
                write_optional(f, "compare-sources workflow disabled", detail)
            }
        }
    }
}

fn write_optional(
    f: &mut fmt::Formatter<'_>,
    prefix: &str,
    detail: &Option<String>,
) -> fmt::Result {
    f.write_str(prefix)?;
    if let Some(detail) = detail {
        write!(f, ": {detail}")?;
    }
    Ok(())
}

impl Error for ComparisonError {}
