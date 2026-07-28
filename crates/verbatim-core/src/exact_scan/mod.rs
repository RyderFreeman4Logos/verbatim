//! Exact filtered scans and full-precision rescoring contract (issue #376).
//!
//! This pure contract module defines the typed boundaries for:
//!
//! - **Exact filtered scan**: contiguous vector extents, sorted ID runs, and
//!   exact small-scope scans; crossover from predicate-aware ANN selected by
//!   *measured* thresholds (not hardcoded).
//! - **Full-precision rescoring**: rescore ANN candidates with exact
//!   original-vector distances (cosine/dot/L2).
//! - **Candidate recall gating**: candidate recall@K vs final recall@K,
//!   reported separately.
//! - **Exact ground truth**: offline/diagnostic exhaustive trusted Top-K.
//! - **Quality policy**: scoped-exact claims, never global; compressed
//!   candidates allowed only if exact-ground-truth gates pass.
//! - **Budget bounds**: top-K memory and I/O batch sizes bounded; typed
//!   exhaustion on budget overrun.
//!
//! It is deliberately a **walking skeleton**: no live SSD I/O, no SIMD kernels,
//! no DiskANN3 binding, no vector math beyond the portable scalar reference
//! kernel. See `docs/architecture/exact-filtered-scans.md`.

mod budget;
mod contract;
mod error;
mod ground_truth;
mod metric;
mod recall;
mod request;
mod rescore;
mod scope;

pub use budget::{BudgetExhaustion, RescoringBudget, RescoringBudgetFields};
pub use contract::{
    select_strategy, AuthorizedScope, CrossoverThreshold, ExactnessClaim, ScanStrategy,
};
pub use error::{ExactScanDiagnosticCode, ExactScanError, ExactScanResult};
pub use ground_truth::{GroundTruthHit, GroundTruthScope, GroundTruthTopK};
pub use metric::{
    reference_distance, ExactMetric, MetricScore, VectorNormalization,
    COSINE_UNIT_LENGTH_TOLERANCE, EXACT_VECTOR_DIMENSION,
};
pub use recall::CandidateRecallReport;
pub use request::{
    ExactScanHit, ExactScanRequest, ExactScanResult_ as ExactScanOutcome, ScanCompleteness,
};
pub use rescore::{
    RescoreCandidate, RescoredCandidate, RescoredPool, RescoringRequest, RescoringResult,
};
pub use scope::{ContiguousExtent, FilterScope, SortedIdRun, VectorOffsetId};

/// Contract schema version for exact-filtered-scan documents.
pub const EXACT_SCAN_CONTRACT_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
#[path = "../exact_scan_tests.rs"]
mod tests;
