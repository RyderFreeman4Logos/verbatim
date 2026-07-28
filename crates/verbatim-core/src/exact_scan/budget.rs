//! Hard caps bounding top-K memory and I/O batch sizes for exact scans.

use serde::{Deserialize, Deserializer, Serialize};

use super::{ExactScanDiagnosticCode, ExactScanError, ExactScanResult};

/// Field bag used to construct and validate a [`RescoringBudget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RescoringBudgetFields {
    /// Maximum number of final results (top-K) retained after ranking.
    pub top_k: u32,
    /// Maximum number of candidate originals that may be fetched for rescoring.
    pub candidate_cap: u32,
    /// Maximum number of original vectors read per I/O batch.
    pub io_batch_size: u32,
}

/// Hard limits on top-K memory and I/O batch sizes, enforced before rescoring.
///
/// Top-K memory is bounded by `top_k`. The candidate cap bounds the number of
/// original vectors fetched from SSD. The I/O batch size bounds the number of
/// vectors read in a single batch. Exhaustion is reported via a typed enum,
/// never by panicking or by an unbounded fallback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct RescoringBudget {
    pub top_k: u32,
    pub candidate_cap: u32,
    pub io_batch_size: u32,
}

impl<'de> Deserialize<'de> for RescoringBudget {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: Deserializer<'de>,
    {
        let fields = RescoringBudgetFields::deserialize(deserializer)?;
        Self::new(fields).map_err(serde::de::Error::custom)
    }
}

impl RescoringBudget {
    /// Builds a budget only when every limit is positively bounded.
    pub fn new(fields: RescoringBudgetFields) -> ExactScanResult<Self> {
        let budget = Self {
            top_k: fields.top_k,
            candidate_cap: fields.candidate_cap,
            io_batch_size: fields.io_batch_size,
        };
        budget.validate()?;
        Ok(budget)
    }

    /// Conservative walking-skeleton defaults.
    pub const fn skeleton_default() -> Self {
        Self {
            top_k: 16,
            candidate_cap: 256,
            io_batch_size: 64,
        }
    }

    /// Revalidates fields after decode or before an adapter creates work.
    pub fn validate(&self) -> ExactScanResult<()> {
        if self.top_k == 0 {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::InvalidTopK,
            ));
        }
        if self.candidate_cap == 0 {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::InvalidBudget,
            ));
        }
        if self.io_batch_size == 0 {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::InvalidIoBatchSize,
            ));
        }
        Ok(())
    }

    /// Validates that a candidate count does not exceed the cap.
    pub fn check_candidate_count(&self, count: usize) -> ExactScanResult<()> {
        if count > self.candidate_cap as usize {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::CandidateCountExceedsCap,
            ));
        }
        Ok(())
    }
}

/// Typed exhaustion state when a budget prevents complete rescoring.
///
/// The exact-scan pipeline reports this type rather than silently truncating.
/// Production rescoring and ground-truth exhaustive paths both surface it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BudgetExhaustion {
    /// Candidate fetch stopped at the cap before all candidates were scored.
    CandidateCapReached,
    /// A top-K heap filled before all candidates were considered.
    TopKHeapFull,
    /// An I/O batch could not be completed within the configured size.
    IoBatchExhausted,
}

impl BudgetExhaustion {
    /// Every exhaustion variant, useful for exhaustive contract tests.
    pub const ALL: [Self; 3] = [
        Self::CandidateCapReached,
        Self::TopKHeapFull,
        Self::IoBatchExhausted,
    ];

    /// Stable machine-readable code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CandidateCapReached => "candidate_cap_reached",
            Self::TopKHeapFull => "top_k_heap_full",
            Self::IoBatchExhausted => "io_batch_exhausted",
        }
    }
}

#[cfg(test)]
mod budget_tests {
    use super::*;

    #[test]
    fn valid_budget_constructs() {
        let b = RescoringBudget::new(RescoringBudgetFields {
            top_k: 10,
            candidate_cap: 100,
            io_batch_size: 32,
        })
        .unwrap();
        assert_eq!(b.top_k, 10);
    }

    #[test]
    fn zero_top_k_rejected() {
        assert!(RescoringBudget::new(RescoringBudgetFields {
            top_k: 0,
            candidate_cap: 100,
            io_batch_size: 32,
        })
        .is_err());
    }

    #[test]
    fn zero_candidate_cap_rejected() {
        assert!(RescoringBudget::new(RescoringBudgetFields {
            top_k: 10,
            candidate_cap: 0,
            io_batch_size: 32,
        })
        .is_err());
    }

    #[test]
    fn zero_io_batch_rejected() {
        assert!(RescoringBudget::new(RescoringBudgetFields {
            top_k: 10,
            candidate_cap: 100,
            io_batch_size: 0,
        })
        .is_err());
    }

    #[test]
    fn candidate_count_within_cap_passes() {
        let b = RescoringBudget::skeleton_default();
        assert!(b.check_candidate_count(10).is_ok());
    }

    #[test]
    fn candidate_count_over_cap_rejected() {
        let b = RescoringBudget::skeleton_default();
        assert!(b
            .check_candidate_count((b.candidate_cap + 1) as usize)
            .is_err());
    }
}
