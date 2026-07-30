//! Deadline, retry, and backpressure markers bound to `SearchBudget`.

use serde::{Deserialize, Serialize};

use crate::search_planner::SearchBudget;

use super::{QdrantBackendDiagnosticCode, QdrantBackendError, QdrantBackendResult};

/// Backpressure signal observed by the adapter control plane.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackpressureMarker {
    None,
    QueueDepthHigh,
    RateLimited,
    StorageThrottled,
}

/// Budget binding for one Qdrant operation with retry and deadline controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QdrantOperationBudget {
    caller_budget: SearchBudget,
    operation_budget: SearchBudget,
    max_retries: u16,
    deadline_micros: u64,
    backpressure: BackpressureMarker,
}

impl QdrantOperationBudget {
    pub fn new(
        caller_budget: SearchBudget,
        operation_budget: SearchBudget,
        max_retries: u16,
        deadline_micros: u64,
        backpressure: BackpressureMarker,
    ) -> QdrantBackendResult<Self> {
        caller_budget.validate().map_err(|_| {
            QdrantBackendError::contract(QdrantBackendDiagnosticCode::InvalidSearchBudget)
        })?;
        operation_budget.validate().map_err(|_| {
            QdrantBackendError::contract(QdrantBackendDiagnosticCode::InvalidSearchBudget)
        })?;
        if !operation_budget.is_not_wider_than(&caller_budget) {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::SearchBudgetWidened,
            ));
        }
        if max_retries == 0 || deadline_micros == 0 {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidSearchBudget,
            ));
        }
        if deadline_micros > operation_budget.fields().max_wall_time_micros {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::SearchBudgetWidened,
            ));
        }
        Ok(Self {
            caller_budget,
            operation_budget,
            max_retries,
            deadline_micros,
            backpressure,
        })
    }

    pub const fn caller_budget(&self) -> SearchBudget {
        self.caller_budget
    }

    pub const fn operation_budget(&self) -> SearchBudget {
        self.operation_budget
    }

    pub const fn max_retries(&self) -> u16 {
        self.max_retries
    }

    pub const fn deadline_micros(&self) -> u64 {
        self.deadline_micros
    }

    pub const fn backpressure(&self) -> BackpressureMarker {
        self.backpressure
    }
}
