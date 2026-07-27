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
            scope_members: self.scope_members.saturating_add(increment.scope_members),
            enumerations: self.enumerations.saturating_add(increment.enumerations),
            candidates: self.candidates.saturating_add(increment.candidates),
            cost_units: self.cost_units.saturating_add(increment.cost_units),
            wall_time_ms: self.wall_time_ms.saturating_add(increment.wall_time_ms),
        };
        for (dimension, used, limit) in [
            (
                ExhaustiveAuditBudgetDimension::ScopeMembers,
                next.scope_members,
                budget.max_scope_members,
            ),
            (
                ExhaustiveAuditBudgetDimension::Enumerations,
                next.enumerations,
                budget.max_enumerations,
            ),
            (
                ExhaustiveAuditBudgetDimension::Candidates,
                next.candidates,
                budget.max_candidates,
            ),
            (
                ExhaustiveAuditBudgetDimension::CostUnits,
                next.cost_units,
                budget.max_cost_units,
            ),
            (
                ExhaustiveAuditBudgetDimension::WallTimeMs,
                next.wall_time_ms,
                budget.max_wall_time_ms,
            ),
        ] {
            if used > limit {
                return Err(ExhaustiveAuditError::BudgetExhausted {
                    exhaustion: ExhaustiveAuditBudgetExhaustion {
                        dimension,
                        limit,
                        used,
                    },
                });
            }
        }
        Ok(next)
    }
}
