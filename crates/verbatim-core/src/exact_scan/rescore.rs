//! ANN rescoring contract: recompute exact distances for ANN candidates.

use super::budget::{BudgetExhaustion, RescoringBudget};
use super::metric::{ExactMetric, MetricScore};
use super::scope::VectorOffsetId;
use super::{ExactScanDiagnosticCode, ExactScanError, ExactScanResult};

/// A candidate produced by an approximate backend, awaiting exact rescoring.
#[derive(Debug, Clone, PartialEq)]
pub struct RescoreCandidate {
    id: VectorOffsetId,
    /// The approximate ordering distance as reported by the backend (lower = closer).
    /// Stored for diagnostics; the exact distance replaces it after rescoring.
    approximate_distance: f32,
}

impl RescoreCandidate {
    /// Builds a validated candidate.
    pub fn new(id: VectorOffsetId, approximate_distance: f32) -> ExactScanResult<Self> {
        if !approximate_distance.is_finite() {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::InvalidDistance,
            ));
        }
        Ok(Self {
            id,
            approximate_distance,
        })
    }

    pub const fn id(&self) -> VectorOffsetId {
        self.id
    }

    pub const fn approximate_distance(&self) -> f32 {
        self.approximate_distance
    }
}

/// A request to rescore a bounded candidate pool with exact original vectors.
#[derive(Debug, Clone)]
pub struct RescoringRequest {
    metric: ExactMetric,
    candidates: Vec<RescoreCandidate>,
    budget: RescoringBudget,
}

impl RescoringRequest {
    /// Builds a request, validating candidate uniqueness and the budget cap.
    pub fn new(
        metric: ExactMetric,
        candidates: Vec<RescoreCandidate>,
        budget: RescoringBudget,
    ) -> ExactScanResult<Self> {
        budget.check_candidate_count(candidates.len())?;
        let mut seen = std::collections::HashSet::with_capacity(candidates.len());
        for candidate in &candidates {
            if !seen.insert(candidate.id) {
                return Err(ExactScanError::contract(
                    ExactScanDiagnosticCode::DuplicateCandidateId,
                ));
            }
        }
        Ok(Self {
            metric,
            candidates,
            budget,
        })
    }

    pub const fn metric(&self) -> ExactMetric {
        self.metric
    }

    pub fn candidates(&self) -> &[RescoreCandidate] {
        &self.candidates
    }

    pub const fn budget(&self) -> RescoringBudget {
        self.budget
    }
}

/// One rescored candidate carrying the exact distance.
#[derive(Debug, Clone, PartialEq)]
pub struct RescoredCandidate {
    id: VectorOffsetId,
    exact_score: MetricScore,
    approximate_distance: f32,
}

impl RescoredCandidate {
    pub fn new(
        id: VectorOffsetId,
        exact_score: MetricScore,
        approximate_distance: f32,
    ) -> ExactScanResult<Self> {
        if !approximate_distance.is_finite() {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::InvalidDistance,
            ));
        }
        Ok(Self {
            id,
            exact_score,
            approximate_distance,
        })
    }

    pub const fn id(&self) -> VectorOffsetId {
        self.id
    }

    pub const fn exact_score(&self) -> MetricScore {
        self.exact_score
    }

    pub const fn approximate_distance(&self) -> f32 {
        self.approximate_distance
    }
}

/// The outcome of a rescoring pass.
#[derive(Debug, Clone, PartialEq)]
pub struct RescoringResult {
    candidates: Vec<RescoredCandidate>,
    /// Number of original vectors actually read from SSD.
    vectors_read: u32,
    /// Total bytes of original vectors read (vectors_read * 4 * 4096).
    bytes_read: u64,
    /// Exact-scoring CPU time in nanoseconds, if measured.
    exact_scoring_nanos: Option<u64>,
    /// `Some` when a budget prevented complete rescoring; `None` if complete.
    exhaustion: Option<BudgetExhaustion>,
    metric: ExactMetric,
}

impl RescoringResult {
    /// Builds a validated rescoring result.
    pub fn new(
        candidates: Vec<RescoredCandidate>,
        vectors_read: u32,
        exact_scoring_nanos: Option<u64>,
        exhaustion: Option<BudgetExhaustion>,
        metric: ExactMetric,
    ) -> ExactScanResult<Self> {
        for candidate in &candidates {
            if candidate.exact_score.metric() != metric {
                return Err(ExactScanError::contract(
                    ExactScanDiagnosticCode::InvalidDistance,
                ));
            }
        }
        let bytes_read = vectors_read as u64 * 4 * super::metric::EXACT_VECTOR_DIMENSION as u64;
        Ok(Self {
            candidates,
            vectors_read,
            bytes_read,
            exact_scoring_nanos,
            exhaustion,
            metric,
        })
    }

    pub fn candidates(&self) -> &[RescoredCandidate] {
        &self.candidates
    }

    pub const fn vectors_read(&self) -> u32 {
        self.vectors_read
    }

    pub const fn bytes_read(&self) -> u64 {
        self.bytes_read
    }

    pub const fn exact_scoring_nanos(&self) -> Option<u64> {
        self.exact_scoring_nanos
    }

    pub const fn exhaustion(&self) -> Option<BudgetExhaustion> {
        self.exhaustion
    }

    pub const fn metric(&self) -> ExactMetric {
        self.metric
    }

    /// Returns `true` when rescoring completed for every candidate without budget exhaustion.
    pub const fn is_complete(&self) -> bool {
        self.exhaustion.is_none()
    }
}

/// How the rescoring path reports its rescored candidate pool to the recall gate.
#[derive(Debug, Clone, PartialEq)]
pub struct RescoredPool {
    ids: Vec<VectorOffsetId>,
}

impl RescoredPool {
    /// Builds a pool from rescored candidate IDs.
    pub fn from_rescored(candidates: &[RescoredCandidate]) -> Self {
        Self {
            ids: candidates.iter().map(|c| c.id()).collect(),
        }
    }

    /// Returns the candidate IDs in this pool.
    pub fn ids(&self) -> &[VectorOffsetId] {
        &self.ids
    }
}

#[cfg(test)]
mod rescore_tests {
    use super::*;

    fn candidates(n: u32) -> Vec<RescoreCandidate> {
        (0..n)
            .map(|i| RescoreCandidate::new(i, (i + 1) as f32).unwrap())
            .collect()
    }

    #[test]
    fn request_rejects_duplicate_ids() {
        let metric = ExactMetric::L2;
        let c1 = RescoreCandidate::new(5, 1.0).unwrap();
        let c2 = RescoreCandidate::new(5, 2.0).unwrap();
        let err = RescoringRequest::new(metric, vec![c1, c2], RescoringBudget::skeleton_default());
        assert!(err.is_err());
    }

    #[test]
    fn request_rejects_count_over_cap() {
        let budget = RescoringBudget::new(RescoringBudgetFields {
            top_k: 2,
            candidate_cap: 3,
            io_batch_size: 1,
        })
        .unwrap();
        let metric = ExactMetric::L2;
        let cs = candidates(4);
        let err = RescoringRequest::new(metric, cs, budget);
        assert!(err.is_err());
    }

    #[test]
    fn candidate_rejects_non_finite_distance() {
        assert!(RescoreCandidate::new(0, f32::NAN).is_err());
        assert!(RescoreCandidate::new(0, f32::INFINITY).is_err());
    }

    use super::super::budget::{RescoringBudget, RescoringBudgetFields};
}
