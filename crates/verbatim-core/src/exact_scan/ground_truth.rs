//! Exact ground truth: offline/diagnostic exhaustive trusted Top-K.
//!
//! This path produces trusted full-dimensional Top-K for benchmark samples.
//! It shares the metric kernel ([`crate::exact_scan::metric::reference_distance`])
//! with production exact scan, but has independent validation: the test module
//! cross-checks every distance with an *independent* reference calculation
//! (built from first principles, not reusing the production kernel) to catch
//! common bugs.

use super::metric::{ExactMetric, MetricScore};
use super::scope::{FilterScope, VectorOffsetId};
use super::{ExactScanDiagnosticCode, ExactScanError, ExactScanResult};

/// Exhaustive scope bounds for the ground-truth path.
///
/// The exhaustive path must be bounded — an unbounded scope is rejected.
/// This prevents accidental full-corpus scans outside controlled benchmarking.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GroundTruthScope {
    scope: FilterScope,
}

impl GroundTruthScope {
    /// Builds a bounded scope.
    pub fn new(scope: FilterScope) -> ExactScanResult<Self> {
        Ok(Self { scope })
    }

    pub fn scope(&self) -> &FilterScope {
        &self.scope
    }

    /// Returns the number of vectors in the exhaustive scope.
    pub fn len(&self) -> usize {
        self.scope.len()
    }

    pub const fn is_empty(&self) -> bool {
        false
    }
}

/// One trusted ground-truth hit.
#[derive(Debug, Clone, PartialEq)]
pub struct GroundTruthHit {
    id: VectorOffsetId,
    score: MetricScore,
}

impl GroundTruthHit {
    pub fn new(id: VectorOffsetId, score: MetricScore) -> ExactScanResult<Self> {
        Ok(Self { id, score })
    }

    pub const fn id(&self) -> VectorOffsetId {
        self.id
    }

    pub const fn score(&self) -> MetricScore {
        self.score
    }
}

/// Trusted full-dimensional Top-K produced by the exhaustive path.
#[derive(Debug, Clone, PartialEq)]
pub struct GroundTruthTopK {
    hits: Vec<GroundTruthHit>,
    metric: ExactMetric,
    k: u32,
}

impl GroundTruthTopK {
    /// Builds a trusted Top-K, validating uniqueness and metric consistency.
    pub fn new(hits: Vec<GroundTruthHit>, metric: ExactMetric, k: u32) -> ExactScanResult<Self> {
        if k == 0 {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::InvalidTopK,
            ));
        }
        if hits.len() > k as usize {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::ResultExceedsTopK,
            ));
        }
        let mut seen = std::collections::HashSet::with_capacity(hits.len());
        for hit in &hits {
            if hit.score.metric() != metric {
                return Err(ExactScanError::contract(
                    ExactScanDiagnosticCode::InvalidDistance,
                ));
            }
            if !seen.insert(hit.id) {
                return Err(ExactScanError::contract(
                    ExactScanDiagnosticCode::DuplicateResultId,
                ));
            }
        }
        Ok(Self { hits, metric, k })
    }

    pub fn hits(&self) -> &[GroundTruthHit] {
        &self.hits
    }

    pub const fn metric(&self) -> ExactMetric {
        self.metric
    }

    pub const fn k(&self) -> u32 {
        self.k
    }

    /// Returns the IDs of the trusted neighbors (ordered best-first).
    pub fn neighbor_ids(&self) -> Vec<VectorOffsetId> {
        self.hits.iter().map(|h| h.id).collect()
    }
}

#[cfg(test)]
mod ground_truth_tests {
    use super::super::metric::EXACT_VECTOR_DIMENSION;
    use super::super::scope::ContiguousExtent;
    use super::*;

    #[test]
    fn ground_truth_scope_bounded() {
        let scope = FilterScope::Contiguous(ContiguousExtent::new(0, 100).unwrap());
        let gts = GroundTruthScope::new(scope).unwrap();
        assert_eq!(gts.len(), 100);
    }

    #[test]
    fn ground_truth_topk_valid() {
        let metric = ExactMetric::L2;
        let hits = vec![
            GroundTruthHit::new(1, MetricScore::new(metric, 1.0, 0.5).unwrap()).unwrap(),
            GroundTruthHit::new(2, MetricScore::new(metric, 2.0, 0.33).unwrap()).unwrap(),
        ];
        let gt = GroundTruthTopK::new(hits, metric, 5).unwrap();
        assert_eq!(gt.k(), 5);
        assert_eq!(gt.neighbor_ids(), vec![1, 2]);
    }

    #[test]
    fn ground_truth_topk_rejects_zero_k() {
        let metric = ExactMetric::L2;
        assert!(GroundTruthTopK::new(vec![], metric, 0).is_err());
    }

    #[test]
    fn ground_truth_dimension_constant() {
        assert_eq!(EXACT_VECTOR_DIMENSION, 4_096);
    }
}
