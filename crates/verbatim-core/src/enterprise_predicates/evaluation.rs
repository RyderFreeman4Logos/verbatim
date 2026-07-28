//! Predicate evaluation: select the candidate-generation path from a typed
//! predicate conjunction and an authorized-selectivity classification.
//!
//! An unsupported strict predicate yields a typed failure, never a global ANN
//! plus best-effort post-filter. Zero authorized candidates short-circuit.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::predicate::EnterprisePredicateConjunction;
use super::{
    EnterprisePredicateDiagnosticCode, EnterprisePredicateError, EnterprisePredicateResult,
    SelectivityClass, SelectivityThresholds,
};

/// Decision on which candidate-generation path applies, or a typed failure.
///
/// No variant carries the authorized-cardinality, raw corpus size, or
/// candidate distance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateGenerationPath {
    /// Zero authorized candidates — return immediately without traversal.
    ZeroAuthorized,
    /// Exact full-dimensional scan over the small authorized subset.
    ExactScan,
    /// Planner-selected exact scan or predicate-aware ANN.
    PlannerSelected,
    /// Predicate-aware DiskANN3 traversal over the broad authorized subset.
    PredicateAwareAnn,
}

/// A typed predicate evaluation outcome.
///
/// Each variant is its own path; there is no separate `path` field because the
/// variant *is* the selected path. This avoids round-tripping an enum through
/// `serde(skip)` (which would require a `Default` path that could disagree
/// with the variant).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum PredicateEvaluation {
    /// Zero authorized candidates; return immediately without vector pages.
    Zero,
    /// Small authorized set: exact full-dimensional scan.
    ExactScan,
    /// Medium authorized set: planner-selected exact or predicate-aware ANN.
    PlannerSelected,
    /// Broad authorized set: predicate-aware DiskANN3 traversal.
    PredicateAwareAnn,
}

impl PredicateEvaluation {
    /// Returns the selected candidate-generation path.
    pub const fn path(self) -> CandidateGenerationPath {
        match self {
            Self::Zero => CandidateGenerationPath::ZeroAuthorized,
            Self::ExactScan => CandidateGenerationPath::ExactScan,
            Self::PlannerSelected => CandidateGenerationPath::PlannerSelected,
            Self::PredicateAwareAnn => CandidateGenerationPath::PredicateAwareAnn,
        }
    }

    /// True when no traversal is permitted.
    pub const fn is_zero(self) -> bool {
        matches!(self, Self::Zero)
    }
}

/// Evaluates a typed predicate conjunction against calibrated selectivity
/// thresholds and the authorized-candidate cardinality to produce the
/// candidate-generation decision.
///
/// `authorized_count` is the cardinality of the authorized subset. If it is
/// zero the decision is `Zero` and no vector pages are touched. An unsupported
/// strict predicate is a typed failure (`UnsupportedStrictPredicate`), never a
/// silent fallback to global ANN with post-filter.
///
/// # Errors
/// - [`EnterprisePredicateDiagnosticCode::UnsupportedStrictPredicate`] when the
///   predicate set contains a strict predicate the index path cannot honour.
/// - [`EnterprisePredicateDiagnosticCode::InvalidSelectivityThreshold`] when
///   thresholds are malformed.
pub fn evaluate_predicates(
    conjunction: &EnterprisePredicateConjunction,
    authorized_count: u64,
    thresholds: &SelectivityThresholds,
) -> EnterprisePredicateResult<PredicateEvaluation> {
    thresholds.validate()?;
    conjunction
        .predicates()
        .iter()
        .try_for_each(|predicate| predicate.validate())?;

    if authorized_count == 0 {
        return Ok(PredicateEvaluation::Zero);
    }

    let class = SelectivityClass::classify(authorized_count, thresholds);
    match class {
        SelectivityClass::Zero => Ok(PredicateEvaluation::Zero),
        SelectivityClass::Small => Ok(PredicateEvaluation::ExactScan),
        SelectivityClass::Medium => Ok(PredicateEvaluation::PlannerSelected),
        SelectivityClass::Broad => Ok(PredicateEvaluation::PredicateAwareAnn),
    }
}

/// Marker type for a strict predicate that the index cannot honour.
///
/// This is a closed, fail-closed signal: rather than silently degrading to
/// global ANN plus best-effort post-filter, the planner must return a typed
/// failure referencing this marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UnsupportedStrictPredicate;

impl UnsupportedStrictPredicate {
    /// Produces the fail-closed typed error for an unsupported strict predicate.
    pub const fn error() -> EnterprisePredicateError {
        EnterprisePredicateError::contract(
            EnterprisePredicateDiagnosticCode::UnsupportedStrictPredicate,
        )
    }
}

impl fmt::Display for UnsupportedStrictPredicate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("unsupported strict predicate (fail-closed)")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::enterprise_predicates::predicate::{EnterpriseLifecycleState, EnterprisePredicate};

    fn thresholds() -> SelectivityThresholds {
        SelectivityThresholds::new(1_024, 8_192).unwrap()
    }

    fn one_predicate() -> EnterprisePredicateConjunction {
        EnterprisePredicateConjunction::new(vec![EnterprisePredicate::source("legal").unwrap()])
            .unwrap()
    }

    #[test]
    fn zero_authorized_returns_immediately() {
        let evaluation = evaluate_predicates(&one_predicate(), 0, &thresholds()).unwrap();
        assert!(evaluation.is_zero());
        assert_eq!(evaluation.path(), CandidateGenerationPath::ZeroAuthorized);
    }

    #[test]
    fn small_authorized_uses_exact_scan() {
        let evaluation = evaluate_predicates(&one_predicate(), 512, &thresholds()).unwrap();
        assert_eq!(evaluation.path(), CandidateGenerationPath::ExactScan);
    }

    #[test]
    fn medium_authorized_uses_planner_selected() {
        let evaluation = evaluate_predicates(&one_predicate(), 4_096, &thresholds()).unwrap();
        assert_eq!(evaluation.path(), CandidateGenerationPath::PlannerSelected);
    }

    #[test]
    fn broad_authorized_uses_predicate_aware_ann() {
        let evaluation = evaluate_predicates(&one_predicate(), 50_000, &thresholds()).unwrap();
        assert_eq!(
            evaluation.path(),
            CandidateGenerationPath::PredicateAwareAnn
        );
    }

    #[test]
    fn single_vector_is_exact_scan() {
        let evaluation = evaluate_predicates(&one_predicate(), 1, &thresholds()).unwrap();
        assert_eq!(evaluation.path(), CandidateGenerationPath::ExactScan);
    }

    #[test]
    fn empty_conjunction_with_nonzero_count_still_classifies() {
        let empty = EnterprisePredicateConjunction::new(vec![]).unwrap();
        let evaluation = evaluate_predicates(&empty, 100_000, &thresholds()).unwrap();
        assert_eq!(
            evaluation.path(),
            CandidateGenerationPath::PredicateAwareAnn
        );
    }

    #[test]
    fn invalid_thresholds_yield_typed_failure() {
        let bad = SelectivityThresholds::new(0, 8_192).unwrap_err();
        assert_eq!(
            bad.diagnostic_code(),
            EnterprisePredicateDiagnosticCode::InvalidSelectivityThreshold
        );
    }

    #[test]
    fn lifecycle_conjunction_evaluates_normally() {
        let conjunction =
            EnterprisePredicateConjunction::new(vec![EnterprisePredicate::lifecycle(
                EnterpriseLifecycleState::Active,
            )])
            .unwrap();
        let evaluation = evaluate_predicates(&conjunction, 100, &thresholds()).unwrap();
        assert_eq!(evaluation.path(), CandidateGenerationPath::ExactScan);
    }

    #[test]
    fn unsupported_strict_predicate_error_is_fail_closed() {
        let err = UnsupportedStrictPredicate::error();
        assert_eq!(
            err.diagnostic_code(),
            EnterprisePredicateDiagnosticCode::UnsupportedStrictPredicate
        );
        let display = format!("{}", UnsupportedStrictPredicate);
        assert!(display.contains("fail-closed"));
    }
}
