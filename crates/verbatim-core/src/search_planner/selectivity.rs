//! Benchmark-derived selectivity and crossover calibration values.

use super::{SearchPlannerDiagnosticCode, SearchPlannerError, SearchPlannerResult};

/// Benchmark-derived crossover thresholds for a named index generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CrossoverThresholds {
    calibration_generation: u64,
    exact_simd_scan_max_matches: u64,
    predicate_aware_diskann3_min_matches: u64,
    exhaustive_enumeration_max_matches: u64,
}

impl CrossoverThresholds {
    /// Creates a non-zero, ordered threshold set bound to one generation.
    pub fn new(
        calibration_generation: u64,
        exact_simd_scan_max_matches: u64,
        predicate_aware_diskann3_min_matches: u64,
        exhaustive_enumeration_max_matches: u64,
    ) -> SearchPlannerResult<Self> {
        let thresholds = Self {
            calibration_generation,
            exact_simd_scan_max_matches,
            predicate_aware_diskann3_min_matches,
            exhaustive_enumeration_max_matches,
        };
        thresholds.validate()?;
        Ok(thresholds)
    }

    /// Revalidates a threshold set before path selection.
    pub fn validate(&self) -> SearchPlannerResult<()> {
        if self.calibration_generation == 0
            || self.exact_simd_scan_max_matches == 0
            || self.predicate_aware_diskann3_min_matches == 0
            || self.exhaustive_enumeration_max_matches == 0
            || self.exact_simd_scan_max_matches > self.predicate_aware_diskann3_min_matches
        {
            Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::InvalidCrossoverThreshold,
            ))
        } else {
            Ok(())
        }
    }

    /// Returns the generation on which the calibration was measured.
    pub const fn calibration_generation(&self) -> u64 {
        self.calibration_generation
    }

    /// Returns the largest authorized subset for the exact SIMD crossover.
    pub const fn exact_simd_scan_max_matches(&self) -> u64 {
        self.exact_simd_scan_max_matches
    }

    /// Returns the first cardinality at which predicate-aware DiskANN3 is calibrated.
    pub const fn predicate_aware_diskann3_min_matches(&self) -> u64 {
        self.predicate_aware_diskann3_min_matches
    }

    /// Returns the largest bounded explicit enumeration permitted by calibration.
    pub const fn exhaustive_enumeration_max_matches(&self) -> u64 {
        self.exhaustive_enumeration_max_matches
    }
}

/// Authorized filter-selectivity class; no class contains a raw corpus size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum SelectivityClass {
    /// The filter may match the full authorized scope.
    OneHundredPercent,
    /// The filter is calibrated near ten percent of its authorized scope.
    TenPercent,
    /// The filter is calibrated near one percent of its authorized scope.
    OnePercent,
    /// The filter is calibrated near one tenth of one percent of its authorized scope.
    PointOnePercent,
    /// The filter is calibrated near one hundredth of one percent of its authorized scope.
    PointZeroOnePercent,
    /// The filter is scoped to one authorized document.
    SingleDocument,
    /// The request requires explicit exhaustive enumeration and coverage accounting.
    Exhaustive,
}

impl SelectivityClass {
    pub(crate) const fn discriminator(self) -> u64 {
        match self {
            Self::OneHundredPercent => 1,
            Self::TenPercent => 2,
            Self::OnePercent => 3,
            Self::PointOnePercent => 4,
            Self::PointZeroOnePercent => 5,
            Self::SingleDocument => 6,
            Self::Exhaustive => 7,
        }
    }
}

/// Selectivity class paired with the benchmark-derived crossover calibration.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SelectivityProfile {
    class: SelectivityClass,
    crossover: CrossoverThresholds,
}

impl SelectivityProfile {
    /// Creates a profile after validating its crossover calibration.
    pub fn new(
        class: SelectivityClass,
        crossover: CrossoverThresholds,
    ) -> SearchPlannerResult<Self> {
        crossover.validate()?;
        Ok(Self { class, crossover })
    }

    /// Revalidates the embedded crossover calibration.
    pub fn validate(&self) -> SearchPlannerResult<()> {
        self.crossover.validate()
    }

    /// Returns the authorized selectivity class.
    pub const fn class(&self) -> SelectivityClass {
        self.class
    }

    /// Returns the benchmark-derived crossover thresholds.
    pub const fn crossover(&self) -> &CrossoverThresholds {
        &self.crossover
    }

    pub fn requires_exhaustive_enumeration(&self) -> bool {
        matches!(self.class, SelectivityClass::Exhaustive)
    }
}
