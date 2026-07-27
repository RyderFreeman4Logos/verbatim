//! Focused enum-reachability and arithmetic-overflow regressions.

use super::*;

#[test]
fn all_lifecycle_availability_alignment_and_stage_variants_are_reachable() {
    for lifecycle in [
        SourceLifecycle::Active,
        SourceLifecycle::Superseded,
        SourceLifecycle::Retired,
        SourceLifecycle::Archived,
    ] {
        assert!(!lifecycle.as_str().is_empty());
    }
    for availability in [
        SourceAvailability::Authorized,
        SourceAvailability::AclDenied,
        SourceAvailability::VersionGone,
        SourceAvailability::Unresolved,
    ] {
        assert!(!availability.as_str().is_empty());
    }
    for alignment in [
        DimensionAlignment::Agreement,
        DimensionAlignment::Difference,
        DimensionAlignment::Conflict,
        DimensionAlignment::Missing,
        DimensionAlignment::Incomparable,
    ] {
        assert!(!alignment.as_str().is_empty());
    }
    for stage in ComparisonStage::all() {
        assert!(!stage.as_str().is_empty());
    }
}

#[test]
fn checked_overflow_has_valid_exhaustion_evidence_and_max_cap_is_rejected() {
    let budget = ComparisonBudget::new(ComparisonBudgetFields {
        max_dimensions: 1,
        max_sources: 2,
        max_candidates: 1,
        max_tokens: u64::MAX - 1,
        max_cost_units: 1,
        max_wall_time_ms: 1,
    })
    .expect("bounded budget");
    let overflow = ComparisonBudgetUsage {
        tokens: u64::MAX,
        ..Default::default()
    }
    .checked_add(
        &ComparisonBudgetUsage {
            tokens: 1,
            ..Default::default()
        },
        &budget,
    )
    .expect_err("checked token overflow exhausts the budget");
    match overflow {
        ComparisonError::BudgetExhausted { exhaustion, .. } => {
            assert_eq!(exhaustion.dimension, ComparisonBudgetDimension::Tokens);
            assert_eq!(exhaustion.limit, u64::MAX - 1);
            assert_eq!(exhaustion.used, u64::MAX);
            exhaustion
                .validate()
                .expect("overflow exhaustion is structurally valid");
        }
        error => panic!("expected budget exhaustion, got {error:?}"),
    }

    let unrepresentable_cap = ComparisonBudget::new(ComparisonBudgetFields {
        max_dimensions: 1,
        max_sources: 2,
        max_candidates: 1,
        max_tokens: u64::MAX,
        max_cost_units: 1,
        max_wall_time_ms: 1,
    });
    assert!(matches!(
        unrepresentable_cap,
        Err(ComparisonError::Validation { .. })
    ));
}
