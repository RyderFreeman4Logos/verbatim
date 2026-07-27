//! Hard budget caps and checked usage accounting.

use serde::{Deserialize, Serialize};

use super::{CitationAuditError, CitationAuditResult};

#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationAuditBudget {
    pub max_claims: u64,
    pub max_candidates: u64,
    pub max_classifications: u64,
    pub max_cost_units: u64,
    pub max_wall_time_ms: u64,
}

#[derive(Clone, PartialEq, Eq)]
pub struct CitationAuditBudgetFields {
    pub max_claims: u64,
    pub max_candidates: u64,
    pub max_classifications: u64,
    pub max_cost_units: u64,
    pub max_wall_time_ms: u64,
}

impl CitationAuditBudget {
    pub fn new(fields: CitationAuditBudgetFields) -> CitationAuditResult<Self> {
        let budget = Self {
            max_claims: fields.max_claims,
            max_candidates: fields.max_candidates,
            max_classifications: fields.max_classifications,
            max_cost_units: fields.max_cost_units,
            max_wall_time_ms: fields.max_wall_time_ms,
        };
        if [
            budget.max_claims,
            budget.max_candidates,
            budget.max_classifications,
            budget.max_cost_units,
            budget.max_wall_time_ms,
        ]
        .contains(&0)
        {
            return Err(CitationAuditError::validation(
                "every citation-audit budget cap must be positive",
            ));
        }
        Ok(budget)
    }

    pub fn skeleton_default() -> Self {
        Self {
            max_claims: 1_000,
            max_candidates: 10_000,
            max_classifications: 1_000,
            max_cost_units: 100_000,
            max_wall_time_ms: 300_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CitationAuditBudgetDimension {
    Claims,
    Candidates,
    Classifications,
    CostUnits,
    WallTimeMs,
}

impl CitationAuditBudgetDimension {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Claims => "claims",
            Self::Candidates => "candidates",
            Self::Classifications => "classifications",
            Self::CostUnits => "cost_units",
            Self::WallTimeMs => "wall_time_ms",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationAuditBudgetExhaustion {
    pub dimension: CitationAuditBudgetDimension,
    pub limit: u64,
    pub used: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CitationAuditUsage {
    pub claims: u64,
    pub candidates: u64,
    pub classifications: u64,
    pub cost_units: u64,
    pub wall_time_ms: u64,
}

impl CitationAuditUsage {
    pub fn checked_add(
        self,
        increment: Self,
        budget: &CitationAuditBudget,
    ) -> CitationAuditResult<Self> {
        Ok(Self {
            claims: checked_dimension_add(
                CitationAuditBudgetDimension::Claims,
                self.claims,
                increment.claims,
                budget.max_claims,
            )?,
            candidates: checked_dimension_add(
                CitationAuditBudgetDimension::Candidates,
                self.candidates,
                increment.candidates,
                budget.max_candidates,
            )?,
            classifications: checked_dimension_add(
                CitationAuditBudgetDimension::Classifications,
                self.classifications,
                increment.classifications,
                budget.max_classifications,
            )?,
            cost_units: checked_dimension_add(
                CitationAuditBudgetDimension::CostUnits,
                self.cost_units,
                increment.cost_units,
                budget.max_cost_units,
            )?,
            wall_time_ms: checked_dimension_add(
                CitationAuditBudgetDimension::WallTimeMs,
                self.wall_time_ms,
                increment.wall_time_ms,
                budget.max_wall_time_ms,
            )?,
        })
    }
}

fn checked_dimension_add(
    dimension: CitationAuditBudgetDimension,
    used: u64,
    increment: u64,
    limit: u64,
) -> CitationAuditResult<u64> {
    let next = used
        .checked_add(increment)
        .ok_or(CitationAuditError::BudgetExhausted {
            exhaustion: CitationAuditBudgetExhaustion {
                dimension,
                limit,
                used: u64::MAX,
            },
        })?;
    if next > limit {
        return Err(CitationAuditError::BudgetExhausted {
            exhaustion: CitationAuditBudgetExhaustion {
                dimension,
                limit,
                used: next,
            },
        });
    }
    Ok(next)
}
