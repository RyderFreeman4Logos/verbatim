//! Ordered stages for the two-sided comparison state machine.

use std::fmt;

use serde::{Deserialize, Serialize};

/// Stages of a bounded compare-sources workflow run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComparisonStage {
    Decomposing,
    Resolving,
    Extracting,
    Aligning,
    Rendering,
    Complete,
    Incomplete,
}

impl ComparisonStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Decomposing => "decomposing",
            Self::Resolving => "resolving",
            Self::Extracting => "extracting",
            Self::Aligning => "aligning",
            Self::Rendering => "rendering",
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Incomplete)
    }

    pub fn all() -> &'static [Self] {
        &[
            Self::Decomposing,
            Self::Resolving,
            Self::Extracting,
            Self::Aligning,
            Self::Rendering,
            Self::Complete,
            Self::Incomplete,
        ]
    }
}

impl fmt::Display for ComparisonStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}
