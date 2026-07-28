//! Selectivity classification for enterprise predicate evaluation.
//!
//! Determines which candidate-generation path is selected based on the size of
//! the authorized candidate set relative to benchmark-derived thresholds.
//! Zero authorized candidates short-circuit before touching vector pages.

use serde::{Deserialize, Serialize};

use super::{
    EnterprisePredicateDiagnosticCode, EnterprisePredicateError, EnterprisePredicateResult,
};

/// Maximum authorized-cardinality we may speak of; absolute values are never
/// reported in diagnostics.
pub const CARDINALITY_REPORT_CEILING: u64 = u32::MAX as u64;

/// Benchmark-derived crossover thresholds between selectivity classes. All
/// thresholds must be non-zero and monotonically non-decreasing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectivityThresholds {
    /// Largest authorized-cardinality that may use exact full-dimensional scan.
    exact_scan_max_matches: u64,
    /// First authorized-cardinality at which predicate-aware DiskANN3 is calibrated.
    predicate_aware_min_matches: u64,
}

impl SelectivityThresholds {
    /// Creates a validated, ordered threshold set.
    pub fn new(
        exact_scan_max_matches: u64,
        predicate_aware_min_matches: u64,
    ) -> EnterprisePredicateResult<Self> {
        let thresholds = Self {
            exact_scan_max_matches,
            predicate_aware_min_matches,
        };
        thresholds.validate()?;
        Ok(thresholds)
    }

    /// Revalidates thresholds before path selection.
    pub fn validate(&self) -> EnterprisePredicateResult<()> {
        if self.exact_scan_max_matches == 0
            || self.predicate_aware_min_matches == 0
            || self.exact_scan_max_matches > self.predicate_aware_min_matches
        {
            return Err(EnterprisePredicateError::contract(
                EnterprisePredicateDiagnosticCode::InvalidSelectivityThreshold,
            ));
        }
        Ok(())
    }

    /// Largest authorized-cardinality for exact SIMD scan.
    pub const fn exact_scan_max_matches(&self) -> u64 {
        self.exact_scan_max_matches
    }

    /// First authorized-cardinality for predicate-aware DiskANN3.
    pub const fn predicate_aware_min_matches(&self) -> u64 {
        self.predicate_aware_min_matches
    }
}

/// Selectivity classification with documented thresholds.
///
/// No class carries a raw corpus size; only the class and its position in the
/// ordered crossover ladder is reported.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SelectivityClass {
    /// Zero authorized candidates — return immediately without traversal.
    Zero,
    /// Small authorized set: exact full-dimensional scan.
    Small,
    /// Medium authorized set: planner-selected exact or predicate-aware ANN.
    Medium,
    /// Broad authorized set: predicate-aware DiskANN3 traversal.
    Broad,
}

impl SelectivityClass {
    /// Classifies an authorized-cardinality against calibrated thresholds.
    ///
    /// `authorized_count` is the cardinality of the *authorized* subset, never
    /// the raw corpus size. A zero count yields [`SelectivityClass::Zero`].
    pub fn classify(authorized_count: u64, thresholds: &SelectivityThresholds) -> Self {
        thresholds
            .validate()
            .expect("selectivity thresholds must be pre-validated by constructor");
        if authorized_count == 0 {
            Self::Zero
        } else if authorized_count >= thresholds.predicate_aware_min_matches() {
            Self::Broad
        } else if authorized_count <= thresholds.exact_scan_max_matches() {
            Self::Small
        } else {
            Self::Medium
        }
    }

    /// Stable discriminator useful for plan identity hashing.
    #[cfg_attr(not(test), allow(dead_code))]
    pub(crate) const fn discriminator(self) -> u64 {
        match self {
            Self::Zero => 0,
            Self::Small => 1,
            Self::Medium => 2,
            Self::Broad => 3,
        }
    }
}

impl SelectivityThresholds {
    /// Sensible default calibration. Exact scan up to 1,024 authorized vectors;
    /// predicate-aware DiskANN3 from 8,192 and up.
    pub const DEFAULT: Self = Self {
        exact_scan_max_matches: 1_024,
        predicate_aware_min_matches: 8_192,
    };
}

#[cfg(test)]
mod tests {
    use super::*;

    fn thresholds() -> SelectivityThresholds {
        SelectivityThresholds::new(1_024, 8_192).unwrap()
    }

    #[test]
    fn zero_authorized_yields_zero_class() {
        let class = SelectivityClass::classify(0, &thresholds());
        assert_eq!(class, SelectivityClass::Zero);
    }

    #[test]
    fn small_authorized_yields_exact_scan() {
        let class = SelectivityClass::classify(1, &thresholds());
        assert_eq!(class, SelectivityClass::Small);
        let class = SelectivityClass::classify(1_024, &thresholds());
        assert_eq!(class, SelectivityClass::Small);
    }

    #[test]
    fn medium_authorized_yields_medium() {
        let class = SelectivityClass::classify(1_025, &thresholds());
        assert_eq!(class, SelectivityClass::Medium);
        let class = SelectivityClass::classify(8_191, &thresholds());
        assert_eq!(class, SelectivityClass::Medium);
    }

    #[test]
    fn broad_authorized_yields_predicate_aware_ann() {
        let class = SelectivityClass::classify(8_192, &thresholds());
        assert_eq!(class, SelectivityClass::Broad);
        let class = SelectivityClass::classify(1_000_000, &thresholds());
        assert_eq!(class, SelectivityClass::Broad);
    }

    #[test]
    fn single_vector_is_small() {
        let class = SelectivityClass::classify(1, &thresholds());
        assert_eq!(class, SelectivityClass::Small);
    }

    #[test]
    fn hundred_percent_authorized_is_broad() {
        let class = SelectivityClass::classify(100_000, &thresholds());
        assert_eq!(class, SelectivityClass::Broad);
    }

    #[test]
    fn zero_exact_threshold_rejected() {
        let result = SelectivityThresholds::new(0, 8_192);
        assert_eq!(
            result.unwrap_err().diagnostic_code(),
            EnterprisePredicateDiagnosticCode::InvalidSelectivityThreshold
        );
    }

    #[test]
    fn zero_predicate_aware_threshold_rejected() {
        let result = SelectivityThresholds::new(1_024, 0);
        assert_eq!(
            result.unwrap_err().diagnostic_code(),
            EnterprisePredicateDiagnosticCode::InvalidSelectivityThreshold
        );
    }

    #[test]
    fn inverted_thresholds_rejected() {
        // exact_max greater than predicate_aware_min is invalid.
        let result = SelectivityThresholds::new(9_000, 8_192);
        assert_eq!(
            result.unwrap_err().diagnostic_code(),
            EnterprisePredicateDiagnosticCode::InvalidSelectivityThreshold
        );
    }

    #[test]
    fn equal_thresholds_allowed() {
        // exact_max == predicate_aware_min collapses medium to empty band.
        let thresholds = SelectivityThresholds::new(8_192, 8_192).unwrap();
        let class = SelectivityClass::classify(8_191, &thresholds);
        assert_eq!(class, SelectivityClass::Small);
        let class = SelectivityClass::classify(8_192, &thresholds);
        assert_eq!(class, SelectivityClass::Broad);
    }

    #[test]
    fn default_thresholds_are_valid() {
        SelectivityThresholds::DEFAULT.validate().unwrap();
    }

    #[test]
    fn discriminator_is_stable_and_ordered() {
        assert_eq!(SelectivityClass::Zero.discriminator(), 0);
        assert_eq!(SelectivityClass::Small.discriminator(), 1);
        assert_eq!(SelectivityClass::Medium.discriminator(), 2);
        assert_eq!(SelectivityClass::Broad.discriminator(), 3);
    }
}
