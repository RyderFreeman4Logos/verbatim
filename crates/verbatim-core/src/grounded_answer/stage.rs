//! Ordered workflow stages for the grounded-answer pipeline.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Ordered stages of the bounded grounded-answer workflow.
///
/// Terminal stages are [`Self::Published`] and [`Self::Abstained`]. Intermediate
/// stages advance only via the state machine in [`super::workflow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkflowStage {
    /// Initial accept of a QueryPlan (policy gates may still refuse).
    Planned,
    /// Hybrid retrieval producing an EvidencePack.
    Retrieving,
    /// ContextPack assembly from evidence (direct vs expanded units).
    Assembling,
    /// AnswerPlan + draft generation under untrusted-evidence delimiters.
    Generating,
    /// Claim-level verification (IDs, quotations, support/conflict).
    Verifying,
    /// Deterministic citation rendering for publishable claims only.
    Rendering,
    /// Successfully published a GroundedAnswer.
    Published,
    /// Typed abstention (never a verified answer).
    Abstained,
}

impl WorkflowStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Planned => "planned",
            Self::Retrieving => "retrieving",
            Self::Assembling => "assembling",
            Self::Generating => "generating",
            Self::Verifying => "verifying",
            Self::Rendering => "rendering",
            Self::Published => "published",
            Self::Abstained => "abstained",
        }
    }

    /// Whether this stage is a terminal outcome (no further advance).
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Published | Self::Abstained)
    }

    /// Whether this stage may still produce a published answer.
    pub fn may_publish(self) -> bool {
        !self.is_terminal() && self != Self::Abstained
    }

    /// All stages in pipeline order (for exhaustive tests / docs).
    pub fn all() -> &'static [WorkflowStage] {
        &[
            Self::Planned,
            Self::Retrieving,
            Self::Assembling,
            Self::Generating,
            Self::Verifying,
            Self::Rendering,
            Self::Published,
            Self::Abstained,
        ]
    }
}

impl fmt::Display for WorkflowStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
