//! Ordered citation-audit stages and injection-origin boundary.

use std::fmt;

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CitationAuditStage {
    Segmenting,
    Retrieving,
    Classifying,
    Validating,
    Aggregating,
    Complete,
    Incomplete,
    Disabled,
}

impl CitationAuditStage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Segmenting => "segmenting",
            Self::Retrieving => "retrieving",
            Self::Classifying => "classifying",
            Self::Validating => "validating",
            Self::Aggregating => "aggregating",
            Self::Complete => "complete",
            Self::Incomplete => "incomplete",
            Self::Disabled => "disabled",
        }
    }

    pub fn is_terminal(self) -> bool {
        matches!(self, Self::Complete | Self::Incomplete | Self::Disabled)
    }
}

impl fmt::Display for CitationAuditStage {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Origin label for text channels. Document/evidence/model text is untrusted
/// data, never a workflow-instruction or tool-control channel.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditTextOrigin {
    WorkflowInstruction,
    PolicyConfig,
    DocumentBody,
    EvidenceText,
    ModelIntermediate,
}

impl AuditTextOrigin {
    pub fn may_alter_workflow_control(self) -> bool {
        matches!(self, Self::WorkflowInstruction | Self::PolicyConfig)
    }
}
