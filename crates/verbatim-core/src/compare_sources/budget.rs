//! Hard caps and fail-closed usage accounting for a comparison run.

use serde::{Deserialize, Serialize};

use super::error::{
    ComparisonBudgetDimension, ComparisonBudgetExhaustion, ComparisonError, ComparisonResultType,
};

/// Hard limits for one two-sided comparison execution.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonBudget {
    pub max_dimensions: u32,
    pub max_sources: u32,
    pub max_candidates: u32,
    pub max_tokens: u64,
    pub max_cost_units: u64,
    pub max_wall_time_ms: u64,
}

/// Construction fields for [`ComparisonBudget`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparisonBudgetFields {
    pub max_dimensions: u32,
    pub max_sources: u32,
    pub max_candidates: u32,
    pub max_tokens: u64,
    pub max_cost_units: u64,
    pub max_wall_time_ms: u64,
}

impl ComparisonBudget {
    pub fn new(fields: ComparisonBudgetFields) -> ComparisonResultType<Self> {
        let budget = Self {
            max_dimensions: fields.max_dimensions,
            max_sources: fields.max_sources,
            max_candidates: fields.max_candidates,
            max_tokens: fields.max_tokens,
            max_cost_units: fields.max_cost_units,
            max_wall_time_ms: fields.max_wall_time_ms,
        };
        budget.validate()?;
        Ok(budget)
    }

    /// Conservative walking-skeleton limits for a two-version comparison.
    pub fn skeleton_default() -> Self {
        Self {
            max_dimensions: 16,
            max_sources: 2,
            max_candidates: 200,
            max_tokens: 50_000,
            max_cost_units: 100,
            max_wall_time_ms: 60_000,
        }
    }

    pub fn validate(&self) -> ComparisonResultType<()> {
        for (field, value) in [
            ("max_dimensions", u64::from(self.max_dimensions)),
            ("max_sources", u64::from(self.max_sources)),
            ("max_candidates", u64::from(self.max_candidates)),
            ("max_tokens", self.max_tokens),
            ("max_cost_units", self.max_cost_units),
            ("max_wall_time_ms", self.max_wall_time_ms),
        ] {
            if value == 0 {
                return Err(ComparisonError::validation(format!(
                    "{field} must be positive"
                )));
            }
        }
        if self.max_sources > 2 {
            return Err(ComparisonError::validation(
                "max_sources must not exceed 2 in the two-sided walking skeleton",
            ));
        }
        if self.max_dimensions == u32::MAX
            || self.max_candidates == u32::MAX
            || self.max_tokens == u64::MAX
            || self.max_cost_units == u64::MAX
            || self.max_wall_time_ms == u64::MAX
        {
            return Err(ComparisonError::validation(
                "budget caps must leave room for exhaustion evidence",
            ));
        }
        Ok(())
    }
}

/// Accumulated usage against a [`ComparisonBudget`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ComparisonBudgetUsage {
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub dimensions: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub sources: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub candidates: u32,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub tokens: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub cost_units: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub wall_time_ms: u64,
}

fn is_zero_u32(value: &u32) -> bool {
    *value == 0
}
fn is_zero_u64(value: &u64) -> bool {
    *value == 0
}

fn budget_overflow(dimension: ComparisonBudgetDimension, limit: u64) -> ComparisonError {
    let Some(used) = limit.checked_add(1) else {
        return ComparisonError::validation(
            "validated budget cap must leave room for overflow exhaustion evidence",
        );
    };
    ComparisonError::budget_exhausted(
        ComparisonBudgetExhaustion::new(dimension, limit, used),
        format!("{} usage addition overflowed", dimension.as_str()),
    )
}

impl ComparisonBudgetUsage {
    /// Add usage without allowing a saturated value to pass a maximum cap.
    pub fn checked_add(
        &self,
        other: &Self,
        budget: &ComparisonBudget,
    ) -> ComparisonResultType<Self> {
        budget.validate()?;
        let usage = Self {
            dimensions: self
                .dimensions
                .checked_add(other.dimensions)
                .ok_or_else(|| {
                    budget_overflow(
                        ComparisonBudgetDimension::Dimensions,
                        u64::from(budget.max_dimensions),
                    )
                })?,
            sources: self.sources.checked_add(other.sources).ok_or_else(|| {
                budget_overflow(
                    ComparisonBudgetDimension::Sources,
                    u64::from(budget.max_sources),
                )
            })?,
            candidates: self
                .candidates
                .checked_add(other.candidates)
                .ok_or_else(|| {
                    budget_overflow(
                        ComparisonBudgetDimension::Candidates,
                        u64::from(budget.max_candidates),
                    )
                })?,
            tokens: self.tokens.checked_add(other.tokens).ok_or_else(|| {
                budget_overflow(ComparisonBudgetDimension::Tokens, budget.max_tokens)
            })?,
            cost_units: self
                .cost_units
                .checked_add(other.cost_units)
                .ok_or_else(|| {
                    budget_overflow(ComparisonBudgetDimension::CostUnits, budget.max_cost_units)
                })?,
            wall_time_ms: self
                .wall_time_ms
                .checked_add(other.wall_time_ms)
                .ok_or_else(|| {
                    budget_overflow(
                        ComparisonBudgetDimension::WallTimeMs,
                        budget.max_wall_time_ms,
                    )
                })?,
        };
        usage.check_against(budget)?;
        Ok(usage)
    }

    /// Check every cap in stable order; the first excess is a typed error.
    pub fn check_against(&self, budget: &ComparisonBudget) -> ComparisonResultType<()> {
        budget.validate()?;
        let checks = [
            (
                ComparisonBudgetDimension::Dimensions,
                u64::from(budget.max_dimensions),
                u64::from(self.dimensions),
            ),
            (
                ComparisonBudgetDimension::Sources,
                u64::from(budget.max_sources),
                u64::from(self.sources),
            ),
            (
                ComparisonBudgetDimension::Candidates,
                u64::from(budget.max_candidates),
                u64::from(self.candidates),
            ),
            (
                ComparisonBudgetDimension::Tokens,
                budget.max_tokens,
                self.tokens,
            ),
            (
                ComparisonBudgetDimension::CostUnits,
                budget.max_cost_units,
                self.cost_units,
            ),
            (
                ComparisonBudgetDimension::WallTimeMs,
                budget.max_wall_time_ms,
                self.wall_time_ms,
            ),
        ];
        for (dimension, limit, used) in checks {
            if used > limit {
                return Err(ComparisonError::budget_exhausted(
                    ComparisonBudgetExhaustion::new(dimension, limit, used),
                    format!("{} {} exceeds limit {limit}", dimension.as_str(), used),
                ));
            }
        }
        Ok(())
    }
}
