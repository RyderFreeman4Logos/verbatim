//! Checked resource accounting for pure diversity-stage construction.

use serde::{Deserialize, Serialize};

use super::{DiversityDiagnosticCode, DiversityError, DiversityResult};

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiversityBudget {
    pub max_raw_candidates: u64,
    pub max_groups: u64,
    pub max_collapsed_members: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiversityBudgetFields {
    pub max_raw_candidates: u64,
    pub max_groups: u64,
    pub max_collapsed_members: u64,
}

impl DiversityBudget {
    pub fn new(fields: DiversityBudgetFields) -> DiversityResult<Self> {
        let budget = Self {
            max_raw_candidates: fields.max_raw_candidates,
            max_groups: fields.max_groups,
            max_collapsed_members: fields.max_collapsed_members,
        };
        if [
            budget.max_raw_candidates,
            budget.max_groups,
            budget.max_collapsed_members,
        ]
        .contains(&0)
        {
            return Err(DiversityError::validation(
                DiversityDiagnosticCode::BudgetCapsMustBePositive,
            ));
        }
        Ok(budget)
    }

    pub fn skeleton_default() -> Self {
        Self {
            max_raw_candidates: 10_000,
            max_groups: 10_000,
            max_collapsed_members: 10_000,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiversityBudgetDimension {
    RawCandidates,
    Groups,
    CollapsedMembers,
}

impl DiversityBudgetDimension {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::RawCandidates => "raw_candidates",
            Self::Groups => "groups",
            Self::CollapsedMembers => "collapsed_members",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiversityBudgetExhaustion {
    pub dimension: DiversityBudgetDimension,
    pub limit: u64,
    pub used: u64,
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiversityUsage {
    pub raw_candidates: u64,
    pub groups: u64,
    pub collapsed_members: u64,
}

impl DiversityUsage {
    pub fn check(self, budget: &DiversityBudget) -> DiversityResult<()> {
        for (dimension, used, limit) in [
            (
                DiversityBudgetDimension::RawCandidates,
                self.raw_candidates,
                budget.max_raw_candidates,
            ),
            (
                DiversityBudgetDimension::Groups,
                self.groups,
                budget.max_groups,
            ),
            (
                DiversityBudgetDimension::CollapsedMembers,
                self.collapsed_members,
                budget.max_collapsed_members,
            ),
        ] {
            if used > limit {
                return Err(DiversityError::BudgetExhausted {
                    exhaustion: DiversityBudgetExhaustion {
                        dimension,
                        limit,
                        used,
                    },
                });
            }
        }
        Ok(())
    }
}
