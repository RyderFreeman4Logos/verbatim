//! Original-vector refinement and candidate-loss reporting contract.

use serde::{Deserialize, Serialize};

use super::{LanceDbBackendDiagnosticCode, LanceDbBackendError, LanceDbBackendResult};

/// Quality requirements for quantized IVF candidate generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LanceDbQualityPlan {
    refine_factor: u16,
    original_vectors_f32_retained: bool,
    full_precision_rescore_required: bool,
}

impl LanceDbQualityPlan {
    pub const MAX_REFINE_FACTOR: u16 = 32;

    pub fn new(
        refine_factor: u16,
        original_vectors_f32_retained: bool,
        full_precision_rescore_required: bool,
    ) -> LanceDbBackendResult<Self> {
        if refine_factor == 0
            || refine_factor > Self::MAX_REFINE_FACTOR
            || !original_vectors_f32_retained
            || !full_precision_rescore_required
        {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::InvalidQualityPlan,
            ));
        }
        Ok(Self {
            refine_factor,
            original_vectors_f32_retained,
            full_precision_rescore_required,
        })
    }

    pub const fn refine_factor(&self) -> u16 {
        self.refine_factor
    }

    pub const fn original_vectors_f32_retained(&self) -> bool {
        self.original_vectors_f32_retained
    }

    pub const fn full_precision_rescore_required(&self) -> bool {
        self.full_precision_rescore_required
    }
}

/// Candidate loss remains observable because refinement cannot recover omitted neighbors.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateLossReport {
    generated_candidates: u32,
    rescored_candidates: u32,
    omitted_ground_truth_neighbors: u32,
}

impl CandidateLossReport {
    pub fn new(
        generated_candidates: u32,
        rescored_candidates: u32,
        omitted_ground_truth_neighbors: u32,
    ) -> LanceDbBackendResult<Self> {
        if generated_candidates == 0
            || rescored_candidates == 0
            || rescored_candidates > generated_candidates
        {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::InvalidCandidateLossReport,
            ));
        }
        Ok(Self {
            generated_candidates,
            rescored_candidates,
            omitted_ground_truth_neighbors,
        })
    }

    pub const fn generated_candidates(&self) -> u32 {
        self.generated_candidates
    }

    pub const fn rescored_candidates(&self) -> u32 {
        self.rescored_candidates
    }

    pub const fn omitted_ground_truth_neighbors(&self) -> u32 {
        self.omitted_ground_truth_neighbors
    }
}
