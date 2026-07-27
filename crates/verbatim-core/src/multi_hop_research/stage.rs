//! Ordered research rounds for the multi-hop research state machine.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Ordered rounds of the bounded multi-hop research workflow.
///
/// Terminal rounds are [`Self::Complete`] and [`Self::Incomplete`]. Intermediate
/// rounds advance only via the state machine in [`super::workflow`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResearchRound {
    /// Structured decomposition of the research question into subquestions.
    Decomposing,
    /// Parallel retrieval of independent subqueries.
    Retrieving,
    /// Coverage / conflict / unresolved-requirement evaluation.
    EvaluatingCoverage,
    /// Bounded corrective round (new subqueries for gaps only).
    CorrectiveRound,
    /// Coverage sufficient; merged ContextPack is publishable as research output.
    Complete,
    /// Coverage insufficient / budget exhausted / fail-closed incomplete.
    Incomplete,
}

impl ResearchRound {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decomposing => "decomposing",
            Self::Retrieving => "retrieving",
            Self::EvaluatingCoverage => "evaluating_coverage",
            Self::CorrectiveRound => "corrective_round",
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
        }
    }

    /// Whether this round is a terminal outcome (no further advance).
    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Incomplete)
    }

    /// Whether this round may still produce a complete merged pack.
    pub fn may_complete(self) -> bool {
        !self.is_terminal()
    }

    /// All rounds in pipeline order (for exhaustive tests / docs).
    pub fn all() -> &'static [ResearchRound] {
        &[
            Self::Decomposing,
            Self::Retrieving,
            Self::EvaluatingCoverage,
            Self::CorrectiveRound,
            Self::Complete,
            Self::Incomplete,
        ]
    }
}

impl fmt::Display for ResearchRound {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
