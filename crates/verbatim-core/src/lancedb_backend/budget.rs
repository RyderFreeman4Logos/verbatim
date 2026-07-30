//! Search-budget binding for LanceDB reference operations.

use crate::search_planner::SearchBudget;

use super::{LanceDbBackendDiagnosticCode, LanceDbBackendError, LanceDbBackendResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LanceDbOperationBudget {
    caller_budget: SearchBudget,
    operation_budget: SearchBudget,
}

impl LanceDbOperationBudget {
    pub fn new(
        caller_budget: SearchBudget,
        operation_budget: SearchBudget,
    ) -> LanceDbBackendResult<Self> {
        caller_budget.validate().map_err(|_| {
            LanceDbBackendError::contract(LanceDbBackendDiagnosticCode::InvalidSearchBudget)
        })?;
        operation_budget.validate().map_err(|_| {
            LanceDbBackendError::contract(LanceDbBackendDiagnosticCode::InvalidSearchBudget)
        })?;
        if !operation_budget.is_not_wider_than(&caller_budget) {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::SearchBudgetWidened,
            ));
        }
        Ok(Self {
            caller_budget,
            operation_budget,
        })
    }

    pub const fn caller_budget(&self) -> SearchBudget {
        self.caller_budget
    }

    pub const fn operation_budget(&self) -> SearchBudget {
        self.operation_budget
    }
}
