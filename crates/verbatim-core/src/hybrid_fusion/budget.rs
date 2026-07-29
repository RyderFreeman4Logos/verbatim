//! Checked candidate-lifecycle accounting for pure fusion-stage construction.
//!
//! Every retriever and fusion stage has an explicit candidate limit (#371). The
//! budget is validated up-front and re-checked whenever a stage output is built.

use serde::{Deserialize, Serialize};

use super::{HybridFusionDiagnosticCode, HybridFusionError, HybridFusionResult};

/// Field bag used to construct and validate a [`FusionBudget`].
///
/// Limits must be positive and monotonically non-increasing across the
/// lifecycle: the fused pool cannot exceed the sum of retriever candidates,
/// the rerank input cannot exceed the fused pool, and the final hydration
/// list cannot exceed the rerank input. This is the boundedness contract of
/// #371 expressed at the type level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FusionBudgetFields {
    pub max_retriever_candidates: u32,
    pub max_fused_pool_size: u32,
    pub max_rerank_input_size: u32,
    pub max_final_hydration_list_size: u32,
    pub max_debug_output_size: u32,
}

/// Hard candidate caps for every fusion stage, enforced before each transition.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct FusionBudget {
    pub max_retriever_candidates: u32,
    pub max_fused_pool_size: u32,
    pub max_rerank_input_size: u32,
    pub max_final_hydration_list_size: u32,
    pub max_debug_output_size: u32,
}

impl<'de> Deserialize<'de> for FusionBudget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = FusionBudgetFields::deserialize(deserializer)?;
        Self::new(fields).map_err(serde::de::Error::custom)
    }
}

impl FusionBudget {
    /// Builds a budget only when every stage is positively and monotonically bounded.
    pub fn new(fields: FusionBudgetFields) -> HybridFusionResult<Self> {
        let budget = Self {
            max_retriever_candidates: fields.max_retriever_candidates,
            max_fused_pool_size: fields.max_fused_pool_size,
            max_rerank_input_size: fields.max_rerank_input_size,
            max_final_hydration_list_size: fields.max_final_hydration_list_size,
            max_debug_output_size: fields.max_debug_output_size,
        };
        budget.validate()?;
        Ok(budget)
    }

    /// A permissive skeleton default used by contract tests and adapters that
    /// have not yet been wired to a real [`crate::search_planner`] budget.
    pub fn skeleton_default() -> Self {
        Self {
            max_retriever_candidates: 10_000,
            max_fused_pool_size: 5_000,
            max_rerank_input_size: 2_000,
            max_final_hydration_list_size: 500,
            max_debug_output_size: 1_000,
        }
    }

    fn validate(&self) -> HybridFusionResult<()> {
        let caps = [
            self.max_retriever_candidates,
            self.max_fused_pool_size,
            self.max_rerank_input_size,
            self.max_final_hydration_list_size,
            self.max_debug_output_size,
        ];
        if caps.contains(&0) {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::BudgetCapsMustBePositive,
            ));
        }
        // Monotonic non-increasing lifecycle ordering.
        if self.max_fused_pool_size > self.max_retriever_candidates
            || self.max_rerank_input_size > self.max_fused_pool_size
            || self.max_final_hydration_list_size > self.max_rerank_input_size
        {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::BudgetCapsMustMonotonic,
            ));
        }
        Ok(())
    }
}

/// A bounded stage dimension whose exhaustion is reported without payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FusionBudgetDimension {
    RetrieverCandidates,
    FusedPool,
    RerankInput,
    FinalHydrationList,
    DebugOutput,
}

impl FusionBudgetDimension {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RetrieverCandidates => "retriever_candidates",
            Self::FusedPool => "fused_pool",
            Self::RerankInput => "rerank_input",
            Self::FinalHydrationList => "final_hydration_list",
            Self::DebugOutput => "debug_output",
        }
    }
}

/// Closed exhaustion record: which dimension, the limit, and the used count.
/// No candidate text, identifiers, or backend payloads are retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FusionBudgetExhaustion {
    pub dimension: FusionBudgetDimension,
    pub limit: u32,
    pub used: u32,
}

/// Per-stage usage counters checked against a [`FusionBudget`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct FusionUsage {
    pub retriever_candidates: u32,
    pub fused_pool: u32,
    pub rerank_input: u32,
    pub final_hydration_list: u32,
    pub debug_output: u32,
}

impl FusionUsage {
    /// Returns `Ok(())` only when every dimension is within its bound.
    pub fn check(self, budget: &FusionBudget) -> HybridFusionResult<()> {
        for (dimension, used, limit) in [
            (
                FusionBudgetDimension::RetrieverCandidates,
                self.retriever_candidates,
                budget.max_retriever_candidates,
            ),
            (
                FusionBudgetDimension::FusedPool,
                self.fused_pool,
                budget.max_fused_pool_size,
            ),
            (
                FusionBudgetDimension::RerankInput,
                self.rerank_input,
                budget.max_rerank_input_size,
            ),
            (
                FusionBudgetDimension::FinalHydrationList,
                self.final_hydration_list,
                budget.max_final_hydration_list_size,
            ),
            (
                FusionBudgetDimension::DebugOutput,
                self.debug_output,
                budget.max_debug_output_size,
            ),
        ] {
            if used > limit {
                return Err(HybridFusionError::BudgetExhausted {
                    exhaustion: FusionBudgetExhaustion {
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
