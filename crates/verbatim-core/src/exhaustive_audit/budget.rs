//! Hard audit budget caps and checked usage accounting.

use serde::{Deserialize, Serialize};

use super::error::{
    ExhaustiveAuditBudgetDimension, ExhaustiveAuditBudgetExhaustion, ExhaustiveAuditError,
    ExhaustiveAuditResult,
};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExhaustiveAuditBudget {
    pub max_scope_members: u64,
    pub max_enumerations: u64,
    pub max_candidates: u64,
    pub max_cost_units: u64,
    pub max_wall_time_ms: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExhaustiveAuditBudgetFields {
    pub max_scope_members: u64,
    pub max_enumerations: u64,
    pub max_candidates: u64,
    pub max_cost_units: u64,
    pub max_wall_time_ms: u64,
}

impl ExhaustiveAuditBudget {
    pub fn new(fields: ExhaustiveAuditBudgetFields) -> ExhaustiveAuditResult<Self> {
        let budget = Self {
            max_scope_members: fields.max_scope_members,
            max_enumerations: fields.max_enumerations,
            max_candidates: fields.max_candidates,
            max_cost_units: fields.max_cost_units,
            max_wall_time_ms: fields.max_wall_time_ms,
        };
        if [
            budget.max_scope_members,
            budget.max_enumerations,
            budget.max_candidates,
            budget.max_cost_units,
            budget.max_wall_time_ms,
        ]
        .contains(&0)
        {
            return Err(ExhaustiveAuditError::validation(
                "every exhaustive-audit budget cap must be positive",
            ));
        }
        Ok(budget)
    }

    pub fn skeleton_default() -> Self {
        Self {
            max_scope_members: 1_000,
            max_enumerations: 100,
            max_candidates: 100_000,
            max_cost_units: 100_000,
            max_wall_time_ms: 300_000,
        }
    }
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExhaustiveAuditUsage {
    pub scope_members: u64,
    pub enumerations: u64,
    pub candidates: u64,
    pub cost_units: u64,
    pub wall_time_ms: u64,
}

impl ExhaustiveAuditUsage {
    pub fn checked_add(
        self,
        increment: &Self,
        budget: &ExhaustiveAuditBudget,
    ) -> ExhaustiveAuditResult<Self> {
        let next = Self {
            scope_members: checked_dimension_add(
                ExhaustiveAuditBudgetDimension::ScopeMembers,
                self.scope_members,
                increment.scope_members,
                budget.max_scope_members,
            )?,
            enumerations: checked_dimension_add(
                ExhaustiveAuditBudgetDimension::Enumerations,
                self.enumerations,
                increment.enumerations,
                budget.max_enumerations,
            )?,
            candidates: checked_dimension_add(
                ExhaustiveAuditBudgetDimension::Candidates,
                self.candidates,
                increment.candidates,
                budget.max_candidates,
            )?,
            cost_units: checked_dimension_add(
                ExhaustiveAuditBudgetDimension::CostUnits,
                self.cost_units,
                increment.cost_units,
                budget.max_cost_units,
            )?,
            wall_time_ms: checked_dimension_add(
                ExhaustiveAuditBudgetDimension::WallTimeMs,
                self.wall_time_ms,
                increment.wall_time_ms,
                budget.max_wall_time_ms,
            )?,
        };
        Ok(next)
    }
}

fn checked_dimension_add(
    dimension: ExhaustiveAuditBudgetDimension,
    used: u64,
    increment: u64,
    limit: u64,
) -> ExhaustiveAuditResult<u64> {
    let next = used
        .checked_add(increment)
        .ok_or(ExhaustiveAuditError::BudgetExhausted {
            exhaustion: ExhaustiveAuditBudgetExhaustion {
                dimension,
                limit,
                used: u64::MAX,
            },
        })?;
    if next > limit {
        return Err(ExhaustiveAuditError::BudgetExhausted {
            exhaustion: ExhaustiveAuditBudgetExhaustion {
                dimension,
                limit,
                used: next,
            },
        });
    }
    Ok(next)
}
