//! Legal stage machine for exhaustive-audit runs.

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AuditStage {
    Declared,
    Enumerating,
    Covering,
    Reconciling,
    Reporting,
    Complete,
    Incomplete,
    UnableToEstablish,
    Blocked,
}

impl AuditStage {
    pub fn is_terminal(self) -> bool {
        matches!(
            self,
            Self::Complete | Self::Incomplete | Self::UnableToEstablish | Self::Blocked
        )
    }
}
