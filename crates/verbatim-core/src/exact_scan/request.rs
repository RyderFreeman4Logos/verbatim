//! Exact filtered-scan request and result contracts.

use serde::{Deserialize, Serialize};

use super::budget::RescoringBudget;
use super::metric::{ExactMetric, MetricScore};
use super::scope::{FilterScope, VectorOffsetId};
use super::{ExactScanDiagnosticCode, ExactScanError, ExactScanResult};

/// A request for an exact scan over a bounded filter scope.
///
/// The query vector must be validated by the caller (or by the scan engine)
/// via [`ExactMetric::validate_vector`] before it enters the scan. The scope
/// bounds the set of vectors that participate. The budget bounds top-K memory
/// and I/O batch sizes.
#[derive(Debug, Clone)]
pub struct ExactScanRequest {
    metric: ExactMetric,
    query_vector: Vec<f32>,
    scope: FilterScope,
    budget: RescoringBudget,
}

impl ExactScanRequest {
    /// Builds a request, validating the query vector against the metric.
    pub fn new(
        metric: ExactMetric,
        query_vector: Vec<f32>,
        scope: FilterScope,
        budget: RescoringBudget,
    ) -> ExactScanResult<Self> {
        metric.validate_vector(&query_vector)?;
        Ok(Self {
            metric,
            query_vector,
            scope,
            budget,
        })
    }

    pub const fn metric(&self) -> ExactMetric {
        self.metric
    }

    pub fn query_vector(&self) -> &[f32] {
        &self.query_vector
    }

    pub fn scope(&self) -> &FilterScope {
        &self.scope
    }

    pub const fn budget(&self) -> RescoringBudget {
        self.budget
    }
}

/// One ranked result from an exact scan.
#[derive(Debug, Clone, PartialEq)]
pub struct ExactScanHit {
    id: VectorOffsetId,
    score: MetricScore,
}

impl ExactScanHit {
    /// Builds a validated hit.
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

/// Whether the exact scan covered the entire authorized scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanCompleteness {
    /// Every vector in the declared scope was scored exactly.
    FullScope,
    /// A budget limit prevented scoring the entire scope.
    PartialScope,
}

impl ScanCompleteness {
    /// Returns `true` only when the scan covered the full scope.
    pub const fn is_exact_claim_eligible(self) -> bool {
        matches!(self, Self::FullScope)
    }
}

/// The result of an exact scan, bounded by top-K and authorized scope.
///
/// When `completeness` is `FullScope`, the result may carry an *exact* label
/// for the enumerated scope. When it is `PartialScope`, no exact claim is
/// permitted — the result is best-effort within the budget.
#[derive(Debug, Clone, PartialEq)]
pub struct ExactScanResult_ {
    hits: Vec<ExactScanHit>,
    completeness: ScanCompleteness,
    metric: ExactMetric,
}

impl ExactScanResult_ {
    /// Builds a result, validating that cardinality does not exceed top-K,
    /// that IDs are unique, and that all scores use the declared metric.
    pub fn new(
        hits: Vec<ExactScanHit>,
        completeness: ScanCompleteness,
        metric: ExactMetric,
        top_k: u32,
    ) -> ExactScanResult<Self> {
        if hits.len() > top_k as usize {
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
        Ok(Self {
            hits,
            completeness,
            metric,
        })
    }

    /// Returns the ranked hits (best first).
    pub fn hits(&self) -> &[ExactScanHit] {
        &self.hits
    }

    pub const fn completeness(&self) -> ScanCompleteness {
        self.completeness
    }

    pub const fn metric(&self) -> ExactMetric {
        self.metric
    }

    /// Returns `true` only when the scan covered the full declared scope.
    pub const fn is_exact_claim_eligible(&self) -> bool {
        self.completeness.is_exact_claim_eligible()
    }
}

#[cfg(test)]
mod request_tests {
    use super::super::budget::RescoringBudget;
    use super::super::scope::ContiguousExtent;
    use super::*;

    fn dim4096() -> Vec<f32> {
        let mut v = vec![0.0_f32; 4096];
        v[0] = 1.0;
        v
    }

    #[test]
    fn request_validates_query_vector() {
        let mut bad = vec![0.0_f32; 4096];
        bad[0] = f32::NAN;
        let scope = FilterScope::Contiguous(ContiguousExtent::new(0, 10).unwrap());
        let budget = RescoringBudget::skeleton_default();
        let result = ExactScanRequest::new(ExactMetric::Cosine, bad, scope, budget);
        assert!(result.is_err());
    }

    #[test]
    fn request_accepts_valid_cosine_vector() {
        let scope = FilterScope::Contiguous(ContiguousExtent::new(0, 10).unwrap());
        let budget = RescoringBudget::skeleton_default();
        let req = ExactScanRequest::new(ExactMetric::Cosine, dim4096(), scope, budget);
        assert!(req.is_ok());
    }

    #[test]
    fn result_rejects_cardinality_over_top_k() {
        let metric = ExactMetric::L2;
        let hits: Vec<ExactScanHit> = (0..5u32)
            .map(|i| {
                ExactScanHit::new(i, MetricScore::new(metric, (i + 1) as f32, 0.5).unwrap())
                    .unwrap()
            })
            .collect();
        let err = ExactScanResult_::new(hits, ScanCompleteness::FullScope, metric, 3);
        assert!(err.is_err());
    }

    #[test]
    fn result_rejects_duplicate_ids() {
        let metric = ExactMetric::L2;
        let s = MetricScore::new(metric, 1.0, 0.5).unwrap();
        let hits = vec![
            ExactScanHit::new(7, s).unwrap(),
            ExactScanHit::new(7, s).unwrap(),
        ];
        let err = ExactScanResult_::new(hits, ScanCompleteness::FullScope, metric, 10);
        assert!(err.is_err());
    }
}
