//! Budget caps and usage accounting for multi-hop research.

use serde::{Deserialize, Serialize};

use super::error::{BudgetDimension, BudgetExhaustion, ResearchError, ResearchResult};
use super::util::{require_positive_u32, require_positive_u64};

/// Declared hard caps for one multi-hop research run.
///
/// All fields are required and positive. Exhaustion is fail-closed via
/// [`ResearchError::BudgetExhausted`] — never silent truncation into a
/// complete status.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResearchBudget {
    /// Maximum retrieval / corrective rounds (including the first).
    pub max_rounds: u32,
    /// Maximum total subqueries across all rounds.
    pub max_subqueries: u32,
    /// Maximum total candidates considered across retrievers.
    pub max_candidates: u32,
    /// Maximum total tokens (input + output accounting).
    pub max_tokens: u64,
    /// Maximum endpoint / retriever / model calls.
    pub max_endpoint_calls: u32,
    /// Opaque cost units (no currency).
    pub max_cost_units: u64,
    /// Wall-clock budget in milliseconds.
    pub max_wall_time_ms: u64,
}

/// Field bundle for [`ResearchBudget::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResearchBudgetFields {
    pub max_rounds: u32,
    pub max_subqueries: u32,
    pub max_candidates: u32,
    pub max_tokens: u64,
    pub max_endpoint_calls: u32,
    pub max_cost_units: u64,
    pub max_wall_time_ms: u64,
}

impl ResearchBudget {
    pub fn new(fields: ResearchBudgetFields) -> ResearchResult<Self> {
        let budget = Self {
            max_rounds: fields.max_rounds,
            max_subqueries: fields.max_subqueries,
            max_candidates: fields.max_candidates,
            max_tokens: fields.max_tokens,
            max_endpoint_calls: fields.max_endpoint_calls,
            max_cost_units: fields.max_cost_units,
            max_wall_time_ms: fields.max_wall_time_ms,
        };
        budget.validate()?;
        Ok(budget)
    }

    /// Conservative default skeleton budget for contract tests / docs.
    pub fn skeleton_default() -> Self {
        Self {
            max_rounds: 3,
            max_subqueries: 12,
            max_candidates: 200,
            max_tokens: 100_000,
            max_endpoint_calls: 24,
            max_cost_units: 1_000,
            max_wall_time_ms: 60_000,
        }
    }

    pub fn validate(&self) -> ResearchResult<()> {
        require_positive_u32("max_rounds", self.max_rounds)?;
        require_positive_u32("max_subqueries", self.max_subqueries)?;
        require_positive_u32("max_candidates", self.max_candidates)?;
        require_positive_u64("max_tokens", self.max_tokens)?;
        require_positive_u32("max_endpoint_calls", self.max_endpoint_calls)?;
        require_positive_u64("max_cost_units", self.max_cost_units)?;
        require_positive_u64("max_wall_time_ms", self.max_wall_time_ms)?;
        Ok(())
    }
}

/// Accumulated usage against a [`ResearchBudget`].
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ResearchBudgetUsage {
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub rounds: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub subqueries: u32,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub candidates: u32,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub tokens: u64,
    #[serde(default, skip_serializing_if = "is_zero_u32")]
    pub endpoint_calls: u32,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub cost_units: u64,
    #[serde(default, skip_serializing_if = "is_zero_u64")]
    pub wall_time_ms: u64,
}

fn is_zero_u32(v: &u32) -> bool {
    *v == 0
}

fn is_zero_u64(v: &u64) -> bool {
    *v == 0
}

impl ResearchBudgetUsage {
    pub fn saturating_add(&self, other: &Self) -> Self {
        Self {
            rounds: self.rounds.saturating_add(other.rounds),
            subqueries: self.subqueries.saturating_add(other.subqueries),
            candidates: self.candidates.saturating_add(other.candidates),
            tokens: self.tokens.saturating_add(other.tokens),
            endpoint_calls: self.endpoint_calls.saturating_add(other.endpoint_calls),
            cost_units: self.cost_units.saturating_add(other.cost_units),
            wall_time_ms: self.wall_time_ms.saturating_add(other.wall_time_ms),
        }
    }

    /// Check usage against budget; first exceeded dimension wins (fail closed).
    pub fn check_against(&self, budget: &ResearchBudget) -> ResearchResult<()> {
        budget.validate()?;
        if self.rounds > budget.max_rounds {
            return Err(ResearchError::budget_exhausted(
                BudgetExhaustion::new(
                    BudgetDimension::Rounds,
                    u64::from(budget.max_rounds),
                    u64::from(self.rounds),
                ),
                format!(
                    "rounds {} exceeds max_rounds {}",
                    self.rounds, budget.max_rounds
                ),
            ));
        }
        if self.subqueries > budget.max_subqueries {
            return Err(ResearchError::budget_exhausted(
                BudgetExhaustion::new(
                    BudgetDimension::Subqueries,
                    u64::from(budget.max_subqueries),
                    u64::from(self.subqueries),
                ),
                format!(
                    "subqueries {} exceeds max_subqueries {}",
                    self.subqueries, budget.max_subqueries
                ),
            ));
        }
        if self.candidates > budget.max_candidates {
            return Err(ResearchError::budget_exhausted(
                BudgetExhaustion::new(
                    BudgetDimension::Candidates,
                    u64::from(budget.max_candidates),
                    u64::from(self.candidates),
                ),
                format!(
                    "candidates {} exceeds max_candidates {}",
                    self.candidates, budget.max_candidates
                ),
            ));
        }
        if self.tokens > budget.max_tokens {
            return Err(ResearchError::budget_exhausted(
                BudgetExhaustion::new(BudgetDimension::Tokens, budget.max_tokens, self.tokens),
                format!(
                    "tokens {} exceeds max_tokens {}",
                    self.tokens, budget.max_tokens
                ),
            ));
        }
        if self.endpoint_calls > budget.max_endpoint_calls {
            return Err(ResearchError::budget_exhausted(
                BudgetExhaustion::new(
                    BudgetDimension::EndpointCalls,
                    u64::from(budget.max_endpoint_calls),
                    u64::from(self.endpoint_calls),
                ),
                format!(
                    "endpoint_calls {} exceeds max_endpoint_calls {}",
                    self.endpoint_calls, budget.max_endpoint_calls
                ),
            ));
        }
        if self.cost_units > budget.max_cost_units {
            return Err(ResearchError::budget_exhausted(
                BudgetExhaustion::new(
                    BudgetDimension::CostUnits,
                    budget.max_cost_units,
                    self.cost_units,
                ),
                format!(
                    "cost_units {} exceeds max_cost_units {}",
                    self.cost_units, budget.max_cost_units
                ),
            ));
        }
        if self.wall_time_ms > budget.max_wall_time_ms {
            return Err(ResearchError::budget_exhausted(
                BudgetExhaustion::new(
                    BudgetDimension::WallTimeMs,
                    budget.max_wall_time_ms,
                    self.wall_time_ms,
                ),
                format!(
                    "wall_time_ms {} exceeds max_wall_time_ms {}",
                    self.wall_time_ms, budget.max_wall_time_ms
                ),
            ));
        }
        Ok(())
    }

    /// Whether a corrective round may still be scheduled under remaining caps.
    pub fn may_start_corrective_round(&self, budget: &ResearchBudget) -> ResearchResult<()> {
        // Corrective round consumes one additional round slot.
        let projected = Self {
            rounds: self.rounds.saturating_add(1),
            ..self.clone()
        };
        projected.check_against(budget)
    }
}
