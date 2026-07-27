//! Ordered, pure stage machine for diversity-report adapters.

use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use super::{DiversityError, DiversityMode, DiversityResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiversityStage {
    Grouping,
    SelectingRepresentatives,
    EmittingCollapseReport,
    Complete,
    Incomplete,
    Disabled,
}

impl DiversityStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Grouping => "grouping",
            Self::SelectingRepresentatives => "selecting_representatives",
            Self::EmittingCollapseReport => "emitting_collapse_report",
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
            Self::Disabled => "disabled",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Incomplete | Self::Disabled)
    }
}

impl std::fmt::Display for DiversityStage {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Mode-typed run state. It cannot be advanced once terminal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiversityRun<M: DiversityMode> {
    current_stage: DiversityStage,
    marker: PhantomData<M>,
}

impl<M: DiversityMode> DiversityRun<M> {
    pub fn new() -> Self {
        Self {
            current_stage: DiversityStage::Grouping,
            marker: PhantomData,
        }
    }

    pub fn current_stage(&self) -> DiversityStage {
        self.current_stage
    }

    pub fn advance(&mut self, to: DiversityStage) -> DiversityResult<()> {
        let from = self.current_stage;
        let legal = matches!(
            (from, to),
            (
                DiversityStage::Grouping,
                DiversityStage::SelectingRepresentatives
            ) | (
                DiversityStage::SelectingRepresentatives,
                DiversityStage::EmittingCollapseReport
            ) | (
                DiversityStage::EmittingCollapseReport,
                DiversityStage::Complete
            ) | (DiversityStage::Grouping, DiversityStage::Incomplete)
                | (
                    DiversityStage::SelectingRepresentatives,
                    DiversityStage::Incomplete
                )
                | (
                    DiversityStage::EmittingCollapseReport,
                    DiversityStage::Incomplete
                )
                | (DiversityStage::Grouping, DiversityStage::Disabled)
        );
        if !legal {
            return Err(DiversityError::IllegalTransition { from, to });
        }
        self.current_stage = to;
        Ok(())
    }
}

impl<M: DiversityMode> Default for DiversityRun<M> {
    fn default() -> Self {
        Self::new()
    }
}
