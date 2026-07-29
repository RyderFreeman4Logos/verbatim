//! Ordered lifecycle stages for hybrid fusion.
//!
//! The lifecycle mirrors the issue's required ordering:
//!
//! ```text
//! bounded retriever candidates
//!   -> generation/auth validation
//!   -> bounded fusion
//!   -> exact/reference precedence rules
//!   -> source/thread/near-duplicate diversity
//!   -> bounded rerank
//!   -> final-limit selection
//!   -> batched evidence hydration
//! ```

use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use super::{FusionMode, HybridFusionError, HybridFusionResult};

/// A single lifecycle stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FusionStage {
    /// Bounded retriever candidate pool assembly.
    RetrieverPool,
    /// Generation / authorization validation of retriever outputs.
    AuthValidation,
    /// Bounded fusion merge (RRF / weighted score / etc.).
    FusionMerge,
    /// Exact/reference precedence rules (must-include references).
    PrecedenceRules,
    /// Source/thread/near-duplicate diversity (integrates #361).
    Diversity,
    /// Bounded rerank input assembly.
    Rerank,
    /// Final-limit selection.
    FinalSelection,
    /// Batched evidence hydration (bounded; final candidates only).
    Hydration,
    /// Terminal success.
    Complete,
    /// Terminal: coverage could not be established or a stage was skipped.
    Incomplete,
    /// Terminal: a stage was disabled by the profile.
    Disabled,
}

impl FusionStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RetrieverPool => "retriever_pool",
            Self::AuthValidation => "auth_validation",
            Self::FusionMerge => "fusion_merge",
            Self::PrecedenceRules => "precedence_rules",
            Self::Diversity => "diversity",
            Self::Rerank => "rerank",
            Self::FinalSelection => "final_selection",
            Self::Hydration => "hydration",
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
            Self::Disabled => "disabled",
        }
    }

    /// Returns `true` for terminal stages.
    pub const fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Incomplete | Self::Disabled)
    }

    /// Returns `true` for stages that operate on bounded rerank/final candidates
    /// only (full text/evidence hydration is never applied to full retriever pools).
    pub const fn is_post_fusion(self) -> bool {
        matches!(self, Self::Rerank | Self::FinalSelection | Self::Hydration)
    }
}

impl std::fmt::Display for FusionStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Mode-typed run state. It cannot be advanced once terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FusionRun<M: FusionMode> {
    current_stage: FusionStage,
    marker: PhantomData<M>,
}

impl<M: FusionMode> FusionRun<M> {
    pub fn new() -> Self {
        Self {
            current_stage: FusionStage::RetrieverPool,
            marker: PhantomData,
        }
    }

    pub const fn current_stage(&self) -> FusionStage {
        self.current_stage
    }

    /// Advances to the next stage, rejecting illegal transitions.
    ///
    /// The legal forward ordering is the lifecycle defined above. Any stage
    /// may transition to `Incomplete` or (for non-terminal stages) `Disabled`.
    pub fn advance(&mut self, to: FusionStage) -> HybridFusionResult<()> {
        let from = self.current_stage;
        if from.is_terminal() {
            return Err(HybridFusionError::IllegalTransition { from, to });
        }
        let legal = legal_transition(from, to);
        if !legal {
            return Err(HybridFusionError::IllegalTransition { from, to });
        }
        self.current_stage = to;
        Ok(())
    }
}

impl<M: FusionMode> Default for FusionRun<M> {
    fn default() -> Self {
        Self::new()
    }
}

fn legal_transition(from: FusionStage, to: FusionStage) -> bool {
    // Any non-terminal stage may degrade to Incomplete.
    if to == FusionStage::Incomplete {
        return !from.is_terminal();
    }
    // Forward lifecycle ordering.
    let forward = matches!(
        (from, to),
        (FusionStage::RetrieverPool, FusionStage::AuthValidation)
            | (FusionStage::AuthValidation, FusionStage::FusionMerge)
            | (FusionStage::FusionMerge, FusionStage::PrecedenceRules)
            | (FusionStage::PrecedenceRules, FusionStage::Diversity)
            | (FusionStage::Diversity, FusionStage::Rerank)
            | (FusionStage::Rerank, FusionStage::FinalSelection)
            | (FusionStage::FinalSelection, FusionStage::Hydration)
            | (FusionStage::Hydration, FusionStage::Complete)
    );
    if forward {
        return true;
    }
    // Disabled is permitted from any non-terminal stage except the final forward
    // step into Complete (which has already committed the lifecycle).
    to == FusionStage::Disabled && from != FusionStage::Complete
}
