//! Capability discovery explicitly rejects adapters that cannot preserve semantics.

use super::{DiskAnn3ServiceDiagnosticCode, DiskAnn3ServiceError, DiskAnn3ServiceResult};

/// Required capability envelope for one versioned service endpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ServiceCapabilities {
    preserves_generation: bool,
    preserves_predicate: bool,
    preserves_budget: bool,
    preserves_deadline: bool,
    preserves_completion: bool,
}

impl ServiceCapabilities {
    pub fn new(
        preserves_generation: bool,
        preserves_predicate: bool,
        preserves_budget: bool,
        preserves_deadline: bool,
        preserves_completion: bool,
    ) -> DiskAnn3ServiceResult<Self> {
        if !preserves_generation
            || !preserves_predicate
            || !preserves_budget
            || !preserves_deadline
            || !preserves_completion
        {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::UnsupportedCapability,
            ));
        }
        Ok(Self {
            preserves_generation,
            preserves_predicate,
            preserves_budget,
            preserves_deadline,
            preserves_completion,
        })
    }

    pub const fn preserves_generation(&self) -> bool {
        self.preserves_generation
    }
    pub const fn preserves_predicate(&self) -> bool {
        self.preserves_predicate
    }
    pub const fn preserves_budget(&self) -> bool {
        self.preserves_budget
    }
    pub const fn preserves_deadline(&self) -> bool {
        self.preserves_deadline
    }
    pub const fn preserves_completion(&self) -> bool {
        self.preserves_completion
    }
}
