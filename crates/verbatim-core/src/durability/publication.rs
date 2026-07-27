//! Checked publication ordering across source, index, task, and cache state.

use serde::{Deserialize, Serialize};

use super::{DurabilityDiagnosticCode, DurabilityError, DurabilityResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PublicationStep {
    SourceReplacement,
    IndexPublication,
    TaskStatus,
    CacheInvalidation,
}

/// Commit-boundary order that prevents derived state from advertising a source
/// generation before that authoritative source replacement is committed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationOrder {
    steps: [PublicationStep; 4],
}

impl PublicationOrder {
    pub const fn new(steps: [PublicationStep; 4]) -> Self {
        Self { steps }
    }

    pub const fn canonical() -> Self {
        Self::new([
            PublicationStep::SourceReplacement,
            PublicationStep::IndexPublication,
            PublicationStep::TaskStatus,
            PublicationStep::CacheInvalidation,
        ])
    }

    pub const fn steps(&self) -> &[PublicationStep; 4] {
        &self.steps
    }

    pub fn validate(&self) -> DurabilityResult<()> {
        if self != &Self::canonical() {
            return Err(DurabilityError::validation(
                DurabilityDiagnosticCode::PublicationOrderInvalid,
            ));
        }
        Ok(())
    }
}
