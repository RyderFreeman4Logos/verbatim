//! Candidate recall gating: candidate recall@K vs final recall@K, reported separately.
//!
//! Rescoring improves the *order* among retrieved candidates but cannot recover
//! a true neighbor that was never in the candidate pool. Candidate Recall@K
//! (how many true neighbors are in the candidate pool) is therefore gated
//! *separately* from Final Recall@K (how many true neighbors are in the final
//! rescored top-K). See issue #376, cross-referencing #266.

use serde::{Deserialize, Serialize};

use super::scope::VectorOffsetId;
use super::{ExactScanDiagnosticCode, ExactScanError, ExactScanResult};

/// A candidate recall report separating candidate-pool recall from final recall.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CandidateRecallReport {
    /// Number of true-neighbors present in the candidate pool, up to K.
    candidate_recall_at_k: u32,
    /// Number of true-neighbors present in the final rescored top-K.
    final_recall_at_k: u32,
    /// The K used for both measurements.
    k: u32,
    /// Number of true-neighbors used as the reference set size (ground-truth top-K size).
    true_neighbor_count: u32,
}

impl CandidateRecallReport {
    /// Builds a report from the candidate pool, the final top-K, and the
    /// trusted ground-truth neighbor set.
    ///
    /// `candidate_ids` are the IDs in the ANN candidate pool (before rescoring).
    /// `final_ids` are the IDs in the final rescored top-K.
    /// `true_neighbor_ids` are the trusted ground-truth neighbors (from the
    /// exhaustive path).
    /// `k` is the K used for both measurements.
    pub fn from_sets(
        candidate_ids: &[VectorOffsetId],
        final_ids: &[VectorOffsetId],
        true_neighbor_ids: &[VectorOffsetId],
        k: u32,
    ) -> ExactScanResult<Self> {
        if k == 0 {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::InvalidTopK,
            ));
        }
        let true_set: std::collections::HashSet<VectorOffsetId> =
            true_neighbor_ids.iter().copied().collect();

        let candidate_recall_at_k = candidate_ids
            .iter()
            .filter(|id| true_set.contains(id))
            .take(k as usize)
            .count() as u32;

        let final_recall_at_k = final_ids
            .iter()
            .filter(|id| true_set.contains(id))
            .take(k as usize)
            .count() as u32;

        Ok(Self {
            candidate_recall_at_k,
            final_recall_at_k,
            k,
            true_neighbor_count: true_neighbor_ids.len() as u32,
        })
    }

    pub const fn candidate_recall_at_k(&self) -> u32 {
        self.candidate_recall_at_k
    }

    pub const fn final_recall_at_k(&self) -> u32 {
        self.final_recall_at_k
    }

    pub const fn k(&self) -> u32 {
        self.k
    }

    pub const fn true_neighbor_count(&self) -> u32 {
        self.true_neighbor_count
    }

    /// Candidate recall as a ratio in `[0.0, 1.0]` (0 if no true neighbors).
    pub fn candidate_recall_ratio(&self) -> f64 {
        if self.true_neighbor_count == 0 {
            0.0
        } else {
            f64::from(self.candidate_recall_at_k) / f64::from(self.true_neighbor_count.min(self.k))
        }
    }

    /// Final recall as a ratio in `[0.0, 1.0]` (0 if no true neighbors).
    pub fn final_recall_ratio(&self) -> f64 {
        if self.true_neighbor_count == 0 {
            0.0
        } else {
            f64::from(self.final_recall_at_k) / f64::from(self.true_neighbor_count.min(self.k))
        }
    }

    /// Returns `true` when rescoring could not have improved final recall
    /// because a true neighbor was absent from the candidate pool.
    ///
    /// When this is `true`, the gap between candidate recall and final recall
    /// cannot be closed by rescoring alone — the candidate generation must
    /// change (e.g. increase oversampling).
    pub fn candidate_pool_is_recall_bottleneck(&self) -> bool {
        self.final_recall_at_k < self.candidate_recall_at_k.min(self.true_neighbor_count)
            || self.candidate_recall_at_k < self.k.min(self.true_neighbor_count)
    }
}

#[cfg(test)]
mod recall_tests {
    use super::*;

    #[test]
    fn full_recall_when_pool_contains_all_true_neighbors() {
        let report =
            CandidateRecallReport::from_sets(&[1, 2, 3, 4], &[1, 2, 3], &[1, 2, 3], 3).unwrap();
        assert_eq!(report.candidate_recall_at_k(), 3);
        assert_eq!(report.final_recall_at_k(), 3);
        assert!((report.candidate_recall_ratio() - 1.0).abs() < 1e-9);
        assert!((report.final_recall_ratio() - 1.0).abs() < 1e-9);
    }

    #[test]
    fn missing_neighbor_in_pool_caps_recall() {
        // True neighbors are [1,2,3,4] but pool only has [1,2,5,6]
        let report =
            CandidateRecallReport::from_sets(&[1, 2, 5, 6], &[1, 2, 5], &[1, 2, 3, 4], 3).unwrap();
        assert_eq!(report.candidate_recall_at_k(), 2);
        assert_eq!(report.final_recall_at_k(), 2);
        // Pool is the bottleneck: neighbor 3 and 4 never entered the pool
        assert!(report.candidate_pool_is_recall_bottleneck());
    }

    #[test]
    fn zero_k_rejected() {
        assert!(CandidateRecallReport::from_sets(&[], &[], &[1], 0).is_err());
    }
}
