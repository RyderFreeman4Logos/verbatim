//! Adaptive `nprobes` contract; narrow filters cannot imply a global fixed probe count.

use serde::{Deserialize, Serialize};

use super::{LanceDbBackendDiagnosticCode, LanceDbBackendError, LanceDbBackendResult};

/// Validated minimum/maximum IVF probe range.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdaptiveProbePlan {
    minimum_nprobes: u16,
    maximum_nprobes: u16,
}

impl AdaptiveProbePlan {
    pub const HARD_MAX_NPROBES: u16 = 1_024;

    pub fn new(minimum_nprobes: u16, maximum_nprobes: u16) -> LanceDbBackendResult<Self> {
        if minimum_nprobes == 0
            || maximum_nprobes < minimum_nprobes
            || maximum_nprobes > Self::HARD_MAX_NPROBES
            || minimum_nprobes == maximum_nprobes
        {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::InvalidProbePlan,
            ));
        }
        Ok(Self {
            minimum_nprobes,
            maximum_nprobes,
        })
    }

    pub const fn minimum_nprobes(&self) -> u16 {
        self.minimum_nprobes
    }

    pub const fn maximum_nprobes(&self) -> u16 {
        self.maximum_nprobes
    }

    /// Selects a bounded probe count from selectivity in parts per million.
    pub fn nprobes_for_selectivity(&self, selectivity_ppm: u32) -> u16 {
        let ppm = selectivity_ppm.min(1_000_000);
        let spread = u32::from(self.maximum_nprobes - self.minimum_nprobes);
        self.minimum_nprobes + ((spread * ppm) / 1_000_000) as u16
    }
}
