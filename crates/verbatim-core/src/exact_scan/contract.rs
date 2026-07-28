//! Quality policy, crossover-selection contract, and exactness claims.

use serde::{Deserialize, Serialize};

use super::metric::ExactMetric;
use super::scope::FilterScope;
use super::{ExactScanDiagnosticCode, ExactScanError, ExactScanResult};

/// The authorized scope enumeration required for an exact or completeness claim.
///
/// An exact claim is never global. It must enumerate the declared authorized
/// scope (the set of vectors that were scored). Compressed-candidate plus
/// rescore must never be labelled as globally exact.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuthorizedScope {
    scope: FilterScope,
    metric: ExactMetric,
}

impl AuthorizedScope {
    /// Builds an authorized scope tied to a metric.
    pub fn new(scope: FilterScope, metric: ExactMetric) -> ExactScanResult<Self> {
        Ok(Self { scope, metric })
    }

    pub fn scope(&self) -> &FilterScope {
        &self.scope
    }

    pub const fn metric(&self) -> ExactMetric {
        self.metric
    }

    /// Returns the number of vectors enumerated by this scope.
    pub fn len(&self) -> usize {
        self.scope.len()
    }

    pub const fn is_empty(&self) -> bool {
        false
    }
}

/// An exact or completeness claim label.
///
/// Only `ScopedExact` carries an [`AuthorizedScope`]. `RescoredApproximate`
/// may never be labelled as exact. `Partial` makes no completeness claim.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExactnessClaim {
    /// Exact over the enumerated authorized scope only.
    ScopedExact(AuthorizedScope),
    /// Compressed candidates rescored with exact vectors — never globally exact.
    RescoredApproximate,
    /// A budget prevented full coverage — no completeness claim.
    Partial,
}

impl ExactnessClaim {
    /// Returns `true` only for scoped-exact claims.
    pub const fn is_scoped_exact(&self) -> bool {
        matches!(self, Self::ScopedExact(_))
    }

    /// Returns `true` if this claim is globally exact — always `false`.
    pub const fn is_global_exact(&self) -> bool {
        false
    }
}

/// A measured crossover threshold selecting between exact scan and predicate-aware ANN.
///
/// Crossover is selected by *measured* thresholds, not hardcoded constants.
/// The threshold represents the filter-scope cardinality below which exact
/// sequential scan is faster and more predictable than graph traversal with
/// random SSD reads.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CrossoverThreshold {
    /// Cardinality below which exact scan is preferred.
    cardinality_limit: u32,
    /// The latency ratio (exact_ms / ann_ms) that was measured to derive the limit.
    measured_ratio: f32,
}

impl CrossoverThreshold {
    /// Builds a threshold, rejecting zero or non-finite ratios.
    pub fn new(cardinality_limit: u32, measured_ratio: f32) -> ExactScanResult<Self> {
        if cardinality_limit == 0 || !measured_ratio.is_finite() || measured_ratio <= 0.0 {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::InvalidBudget,
            ));
        }
        Ok(Self {
            cardinality_limit,
            measured_ratio,
        })
    }

    pub const fn cardinality_limit(&self) -> u32 {
        self.cardinality_limit
    }

    pub const fn measured_ratio(&self) -> f32 {
        self.measured_ratio
    }

    /// Returns `true` when the exact path is preferred for this cardinality.
    pub const fn prefers_exact_for(&self, cardinality: u32) -> bool {
        cardinality <= self.cardinality_limit
    }
}

/// The selected scan strategy for a filter scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanStrategy {
    /// Sequential full-precision scan over the scope.
    ExactScan,
    /// Predicate-aware approximate nearest-neighbour traversal.
    PredicateAwareAnn,
}

/// Selects a scan strategy based on a measured crossover threshold.
pub fn select_strategy(scope: &FilterScope, threshold: &CrossoverThreshold) -> ScanStrategy {
    if threshold.prefers_exact_for(scope.len() as u32) {
        ScanStrategy::ExactScan
    } else {
        ScanStrategy::PredicateAwareAnn
    }
}

#[cfg(test)]
mod contract_tests {
    use super::super::scope::ContiguousExtent;
    use super::*;

    #[test]
    fn scoped_exact_claim_carries_scope() {
        let scope = FilterScope::Contiguous(ContiguousExtent::new(0, 10).unwrap());
        let authorized = AuthorizedScope::new(scope, ExactMetric::Cosine).unwrap();
        let claim = ExactnessClaim::ScopedExact(authorized);
        assert!(claim.is_scoped_exact());
        assert!(!claim.is_global_exact());
    }

    #[test]
    fn rescored_approximate_is_not_exact() {
        let claim = ExactnessClaim::RescoredApproximate;
        assert!(!claim.is_scoped_exact());
        assert!(!claim.is_global_exact());
    }

    #[test]
    fn crossover_threshold_valid() {
        let t = CrossoverThreshold::new(500, 0.8).unwrap();
        assert_eq!(t.cardinality_limit(), 500);
        assert!(t.prefers_exact_for(500));
        assert!(!t.prefers_exact_for(501));
    }

    #[test]
    fn crossover_threshold_rejects_invalid() {
        assert!(CrossoverThreshold::new(0, 1.0).is_err());
        assert!(CrossoverThreshold::new(100, 0.0).is_err());
        assert!(CrossoverThreshold::new(100, -1.0).is_err());
        assert!(CrossoverThreshold::new(100, f32::NAN).is_err());
    }

    #[test]
    fn select_strategy_small_scope_picks_exact() {
        let scope = FilterScope::Contiguous(ContiguousExtent::new(0, 100).unwrap());
        let threshold = CrossoverThreshold::new(500, 0.8).unwrap();
        assert_eq!(select_strategy(&scope, &threshold), ScanStrategy::ExactScan);
    }

    #[test]
    fn select_strategy_large_scope_picks_ann() {
        let scope = FilterScope::Contiguous(ContiguousExtent::new(0, 1_000_000).unwrap());
        let threshold = CrossoverThreshold::new(500, 0.8).unwrap();
        assert_eq!(
            select_strategy(&scope, &threshold),
            ScanStrategy::PredicateAwareAnn
        );
    }
}
