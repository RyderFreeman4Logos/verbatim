//! Visible bounded candidate-generation and rescoring profile values.

use super::{SearchPlannerDiagnosticCode, SearchPlannerError, SearchPlannerResult};

/// Quantizer used only for approximate candidate generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CandidateQuantizer {
    /// Product-quantized candidate representation.
    ProductQuantized,
    /// Scalar-quantized candidate representation.
    ScalarQuantized,
}

/// Visible bounded parameters of approximate candidate generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QuantizedCandidateGenerationProfile {
    quantizer: CandidateQuantizer,
    candidate_limit: u32,
    beam_width: u32,
    probe_count: u32,
}

impl QuantizedCandidateGenerationProfile {
    pub(crate) fn new(
        quantizer: CandidateQuantizer,
        candidate_limit: u32,
        beam_width: u32,
        probe_count: u32,
    ) -> SearchPlannerResult<Self> {
        if candidate_limit == 0 || beam_width == 0 || probe_count == 0 {
            Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::InvalidPlan,
            ))
        } else {
            Ok(Self {
                quantizer,
                candidate_limit,
                beam_width,
                probe_count,
            })
        }
    }

    /// Returns the quantizer that generated approximate candidates.
    pub const fn quantizer(&self) -> CandidateQuantizer {
        self.quantizer
    }

    /// Returns the hard candidate-generation cap.
    pub const fn candidate_limit(&self) -> u32 {
        self.candidate_limit
    }

    /// Returns the bounded traversal beam width.
    pub const fn beam_width(&self) -> u32 {
        self.beam_width
    }

    /// Returns the bounded probe count.
    pub const fn probe_count(&self) -> u32 {
        self.probe_count
    }
}

/// Separate full-precision rescoring allocation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullPrecisionRescoreBudget {
    candidate_limit: u32,
    cpu_micros: u64,
}

impl FullPrecisionRescoreBudget {
    pub(crate) fn new(candidate_limit: u32, cpu_micros: u64) -> SearchPlannerResult<Self> {
        if candidate_limit == 0 || cpu_micros == 0 {
            Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::InvalidPlan,
            ))
        } else {
            Ok(Self {
                candidate_limit,
                cpu_micros,
            })
        }
    }

    /// Returns the number of candidates eligible for full-precision rescoring.
    pub const fn candidate_limit(&self) -> u32 {
        self.candidate_limit
    }

    /// Returns the CPU portion reserved for full-precision rescoring.
    pub const fn cpu_micros(&self) -> u64 {
        self.cpu_micros
    }
}
