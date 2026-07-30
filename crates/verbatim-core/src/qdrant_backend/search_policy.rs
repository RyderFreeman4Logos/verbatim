//! Qdrant-primary selection policy: no unconditional local dense pre-search.

use std::sync::atomic::{AtomicU64, Ordering};

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

/// Opaque proof that a Qdrant-primary attempt produced a typed failure.
///
/// Callers cannot construct this value directly. It is emitted only by
/// [`QdrantSearchPolicy::record_typed_qdrant_failure`] after a Qdrant-primary
/// policy has validated the remaining budget for a potential fallback.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QdrantFailureReceipt {
    failure: TypedQdrantFailure,
    remaining_budget: SearchBudget,
    /// Private binding: prevents use with another primary policy.
    policy_seal: PolicySeal,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PolicySeal(u64);

static NEXT_POLICY_SEAL: AtomicU64 = AtomicU64::new(1);

fn next_policy_seal() -> QdrantBackendResult<PolicySeal> {
    NEXT_POLICY_SEAL
        .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |seal| {
            seal.checked_add(1)
        })
        .map(PolicySeal)
        .map_err(|_| QdrantBackendError::contract(QdrantBackendDiagnosticCode::InvalidSearchPolicy))
}

impl QdrantFailureReceipt {
    pub const fn failure(&self) -> TypedQdrantFailure {
        self.failure
    }

    pub const fn remaining_budget(&self) -> SearchBudget {
        self.remaining_budget
    }
}

/// Validated Qdrant-primary search policy.
///
/// Local fallback cannot be pre-authorized. A caller starts with
/// [`QdrantSearchPolicy::qdrant_primary_only`], records a typed Qdrant failure
/// via [`Self::record_typed_qdrant_failure`] (which yields an opaque receipt),
/// then authorizes fallback only with that receipt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QdrantSearchPolicy {
    local_dense: LocalDenseParticipation,
    remaining_budget: SearchBudget,
    typed_failure: Option<TypedQdrantFailure>,
    policy_seal: PolicySeal,
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
            policy_seal: next_policy_seal()?,
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

    /// Records a typed failure from a Qdrant-primary attempt.
    ///
    /// This is the only way to obtain a [`QdrantFailureReceipt`]. The policy must
    /// still be Qdrant-primary (no local dense yet). The remaining budget must be
    /// no wider than the policy budget and must still have capacity for one
    /// fallback attempt.
    pub fn record_typed_qdrant_failure(
        &self,
        typed_failure: TypedQdrantFailure,
        remaining_budget: SearchBudget,
    ) -> QdrantBackendResult<QdrantFailureReceipt> {
        remaining_budget.validate().map_err(|_| {
            QdrantBackendError::contract(QdrantBackendDiagnosticCode::InvalidSearchBudget)
        })?;
        if !matches!(self.local_dense, LocalDenseParticipation::Forbidden) {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidSearchPolicy,
            ));
        }
        if self.typed_failure.is_some() {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidSearchPolicy,
            ));
        }
        if !remaining_budget.is_not_wider_than(&self.remaining_budget) {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::SearchBudgetWidened,
            ));
        }
        // Primary attempt already consumed one stage; fallback needs a later attempt.
        if remaining_budget.fields().max_stage_attempts
            >= self.remaining_budget.fields().max_stage_attempts
            || remaining_budget.fields().max_wall_time_micros == 0
            || remaining_budget.fields().dense_candidate_limit == 0
        {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::FallbackBudgetExhausted,
            ));
        }
        Ok(QdrantFailureReceipt {
            failure: typed_failure,
            remaining_budget,
            policy_seal: self.policy_seal,
        })
    }

    /// Transitions this policy into fallback-enabled state using a sealed receipt.
    ///
    /// The receipt must have been produced from this policy's budget binding via
    /// [`Self::record_typed_qdrant_failure`]. Callers cannot fabricate the receipt.
    pub fn enable_fallback_after_receipt(
        &self,
        receipt: &QdrantFailureReceipt,
    ) -> QdrantBackendResult<Self> {
        if !matches!(self.local_dense, LocalDenseParticipation::Forbidden) {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidSearchPolicy,
            ));
        }
        if receipt.policy_seal != self.policy_seal {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::FallbackWithoutTypedFailure,
            ));
        }
        if !receipt
            .remaining_budget
            .is_not_wider_than(&self.remaining_budget)
        {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::SearchBudgetWidened,
            ));
        }
        Ok(Self {
            local_dense: LocalDenseParticipation::FallbackAfterTypedFailure,
            remaining_budget: receipt.remaining_budget,
            typed_failure: Some(receipt.failure),
            policy_seal: self.policy_seal,
        })
    }

    /// Attempts to authorize a local dense fallback attempt using a sealed receipt.
    pub fn authorize_local_fallback(
        &self,
        receipt: &QdrantFailureReceipt,
    ) -> QdrantBackendResult<()> {
        if receipt.policy_seal != self.policy_seal {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::FallbackWithoutTypedFailure,
            ));
        }
        if !receipt
            .remaining_budget
            .is_not_wider_than(&self.remaining_budget)
        {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::SearchBudgetWidened,
            ));
        }
        match self.local_dense {
            LocalDenseParticipation::Forbidden => {
                // Primary-only policy: authorize only after enabling fallback with the receipt.
                // Direct authorize without enable is forbidden to prevent silent pre-search.
                Err(QdrantBackendError::contract(
                    QdrantBackendDiagnosticCode::FallbackWithoutTypedFailure,
                ))
            }
            LocalDenseParticipation::FallbackAfterTypedFailure => {
                if self.typed_failure != Some(receipt.failure) {
                    return Err(QdrantBackendError::contract(
                        QdrantBackendDiagnosticCode::FallbackWithoutTypedFailure,
                    ));
                }
                if receipt.remaining_budget.fields().max_stage_attempts < 1
                    || receipt.remaining_budget.fields().max_wall_time_micros == 0
                    || receipt.remaining_budget.fields().dense_candidate_limit == 0
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
