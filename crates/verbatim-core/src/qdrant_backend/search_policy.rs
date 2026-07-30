//! Qdrant-primary selection policy: no unconditional local dense pre-search.

use serde::{Deserialize, Serialize};

use crate::search_planner::SearchBudget;

use super::{QdrantBackendDiagnosticCode, QdrantBackendError, QdrantBackendResult};

/// How local dense search may participate relative to Qdrant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LocalDenseParticipation {
    /// Local dense is forbidden for this request path.
    Forbidden,
    /// Local dense may run only after a typed Qdrant failure and remaining budget.
    FallbackAfterTypedFailure,
}

/// Closed set of typed Qdrant failures that may authorize a local fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TypedQdrantFailure {
    TransportUnavailable,
    CollectionNotReady,
    DeadlineExceeded,
    Backpressure,
    CapabilityMissing,
}

/// Non-representable as a compliant policy: unconditional local pre-search.
///
/// This type exists only so tests and docs can name the forbidden pattern; there is
/// no constructor that yields a compliant [`QdrantSearchPolicy`] with this mode.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ForbiddenLocalPreSearch {
    UnconditionalLocalDensePreSearch,
}

/// Validated Qdrant-primary search policy.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QdrantSearchPolicy {
    local_dense: LocalDenseParticipation,
    remaining_budget: SearchBudget,
    typed_failure: Option<TypedQdrantFailure>,
}

impl QdrantSearchPolicy {
    /// Constructs a Qdrant-primary policy with no local dense participation.
    pub fn qdrant_primary_only(remaining_budget: SearchBudget) -> QdrantBackendResult<Self> {
        remaining_budget.validate().map_err(|_| {
            QdrantBackendError::contract(QdrantBackendDiagnosticCode::InvalidSearchBudget)
        })?;
        Ok(Self {
            local_dense: LocalDenseParticipation::Forbidden,
            remaining_budget,
            typed_failure: None,
        })
    }

    /// Local fallback is allowed only after a typed Qdrant failure and remaining budget.
    pub fn fallback_after_typed_failure(
        remaining_budget: SearchBudget,
        typed_failure: TypedQdrantFailure,
    ) -> QdrantBackendResult<Self> {
        remaining_budget.validate().map_err(|_| {
            QdrantBackendError::contract(QdrantBackendDiagnosticCode::InvalidSearchBudget)
        })?;
        if remaining_budget.fields().max_stage_attempts < 2 {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::FallbackBudgetExhausted,
            ));
        }
        if remaining_budget.fields().max_wall_time_micros == 0
            || remaining_budget.fields().dense_candidate_limit == 0
        {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::FallbackBudgetExhausted,
            ));
        }
        Ok(Self {
            local_dense: LocalDenseParticipation::FallbackAfterTypedFailure,
            remaining_budget,
            typed_failure: Some(typed_failure),
        })
    }

    /// Explicit reject path for the historical unconditional local pre-search anti-pattern.
    pub fn reject_unconditional_local_pre_search(
        _forbidden: ForbiddenLocalPreSearch,
    ) -> QdrantBackendResult<Self> {
        Err(QdrantBackendError::contract(
            QdrantBackendDiagnosticCode::UnconditionalLocalPreSearchForbidden,
        ))
    }

    /// Attempts to authorize a local dense fallback attempt.
    pub fn authorize_local_fallback(
        &self,
        remaining_budget: SearchBudget,
        typed_failure: TypedQdrantFailure,
    ) -> QdrantBackendResult<()> {
        remaining_budget.validate().map_err(|_| {
            QdrantBackendError::contract(QdrantBackendDiagnosticCode::InvalidSearchBudget)
        })?;
        if !remaining_budget.is_not_wider_than(&self.remaining_budget) {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::SearchBudgetWidened,
            ));
        }
        match self.local_dense {
            LocalDenseParticipation::Forbidden => Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::UnconditionalLocalPreSearchForbidden,
            )),
            LocalDenseParticipation::FallbackAfterTypedFailure => {
                if self.typed_failure != Some(typed_failure) {
                    return Err(QdrantBackendError::contract(
                        QdrantBackendDiagnosticCode::FallbackWithoutTypedFailure,
                    ));
                }
                if remaining_budget.fields().max_stage_attempts < 1
                    || remaining_budget.fields().max_wall_time_micros == 0
                    || remaining_budget.fields().dense_candidate_limit == 0
                {
                    return Err(QdrantBackendError::contract(
                        QdrantBackendDiagnosticCode::FallbackBudgetExhausted,
                    ));
                }
                Ok(())
            }
        }
    }

    pub const fn local_dense(&self) -> LocalDenseParticipation {
        self.local_dense
    }

    pub const fn remaining_budget(&self) -> SearchBudget {
        self.remaining_budget
    }

    pub const fn typed_failure(&self) -> Option<TypedQdrantFailure> {
        self.typed_failure
    }

    /// Returns true only for compliant Qdrant-primary paths (never unconditional pre-search).
    pub const fn is_qdrant_primary(&self) -> bool {
        matches!(
            self.local_dense,
            LocalDenseParticipation::Forbidden | LocalDenseParticipation::FallbackAfterTypedFailure
        )
    }
}
