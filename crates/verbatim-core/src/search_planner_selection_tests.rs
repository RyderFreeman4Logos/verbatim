use crate::search_planner::{
    BackendCapability, BackendCapabilityFields, CardinalityConfidence, CardinalityEstimate,
    CardinalityFreshness, CardinalitySource, ColdCacheBehavior, CrossoverThresholds,
    EstimateHandlingMode, EstimateReliability, GenerationBinding, MemoryTierBehavior,
    NamedDegradedProfile, PlannedCompleteness, PlannerRequest, RetrievalIntent, RetrievalPlan,
    SearchBudget, SearchBudgetFields, SearchPlanner, SearchPlannerContract,
    SearchPlannerDiagnosticCode, SearchPlannerError, SelectedPath, SelectivityClass,
    SelectivityProfile, StrictPredicateHandlingMode, VectorMetric,
};

fn must_ok<T>(result: Result<T, SearchPlannerError>) -> T {
    match result {
        Ok(value) => value,
        Err(error) => panic!("unexpected planner error: {error:?}"),
    }
}

fn budget() -> SearchBudget {
    must_ok(SearchBudget::new(SearchBudgetFields {
        result_limit: 10,
        dense_candidate_limit: 64,
        lexical_candidate_limit: 64,
        exact_candidate_limit: 128,
        graph_candidate_limit: 32,
        fused_pool_limit: 128,
        rerank_candidate_limit: 64,
        full_precision_rescore_limit: 32,
        hydration_limit: 10,
        max_ssd_pages: 16,
        max_bytes_read: 1_048_576,
        max_cpu_micros: 50_000,
        max_work_units: 10_000,
        max_wall_time_micros: 100_000,
        max_concurrent_stages: 2,
        max_stage_attempts: 3,
        debug_record_limit: 16,
    }))
}

fn capability_fields() -> BackendCapabilityFields {
    BackendCapabilityFields {
        supported_metrics: vec![VectorMetric::Cosine, VectorMetric::DotProduct],
        min_dimension: 1,
        max_dimension: 4_096,
        exact_simd_scan: true,
        supports_pre_rank_predicates: true,
        supports_in_traversal_predicates: true,
        supports_post_filter_predicates: true,
        supports_multi_source: true,
        supports_collection: true,
        supports_tenant: true,
        supports_acl: true,
        supports_pagination: true,
        supports_range: true,
        supports_updates: true,
        supports_deletes: true,
        binds_generation: true,
        max_safe_candidates: 1_000,
        max_safe_pages: 1_000,
        max_safe_bytes: 10_000_000,
        max_safe_cpu_micros: 1_000_000,
        max_safe_work_units: 1_000_000,
        max_safe_concurrent_stages: 8,
        reports_pages_read: true,
        reports_bytes_read: true,
        reports_cpu_micros: true,
        reports_work_units: true,
        cold_cache_behavior: ColdCacheBehavior::Bounded,
        memory_tier_behavior: MemoryTierBehavior::SsdResident,
    }
}

fn capability() -> BackendCapability {
    must_ok(BackendCapability::new(capability_fields()))
}

fn generation() -> GenerationBinding {
    must_ok(GenerationBinding::new(5))
}

fn profile(class: SelectivityClass) -> SelectivityProfile {
    let crossover = must_ok(CrossoverThresholds::new(5, 100, 101, 1_000));
    must_ok(SelectivityProfile::new(class, crossover))
}

fn estimate(
    count: u64,
    freshness: CardinalityFreshness,
    reliability: EstimateReliability,
) -> CardinalityEstimate {
    CardinalityEstimate::new(
        count,
        CardinalityConfidence::High,
        freshness,
        CardinalitySource::AuthorizedCatalog,
        reliability,
    )
}

fn plan_for(
    count: u64,
    class: SelectivityClass,
    freshness: CardinalityFreshness,
    reliability: EstimateReliability,
    estimate_handling: EstimateHandlingMode,
    strict_handling: StrictPredicateHandlingMode,
    capability: BackendCapability,
    intent: RetrievalIntent,
) -> Result<RetrievalPlan, SearchPlannerError> {
    let request = PlannerRequest::new(
        budget(),
        estimate(count, freshness, reliability),
        profile(class),
        capability,
        generation(),
        intent,
        estimate_handling,
        true,
        strict_handling,
        4_096,
        VectorMetric::Cosine,
    )?;
    SearchPlanner.plan_and_validate(request)
}

#[test]
fn cardinalities_from_zero_through_millions_preserve_final_top_k() {
    let cases = [
        (0, SelectivityClass::OneHundredPercent),
        (1, SelectivityClass::SingleDocument),
        (10, SelectivityClass::TenPercent),
        (100, SelectivityClass::OnePercent),
        (10_000, SelectivityClass::PointOnePercent),
        (1_000_000, SelectivityClass::PointZeroOnePercent),
    ];

    for (count, class) in cases {
        let plan = must_ok(plan_for(
            count,
            class,
            CardinalityFreshness::Fresh,
            EstimateReliability::Verified,
            EstimateHandlingMode::FailClosed,
            StrictPredicateHandlingMode::FailClosed,
            capability(),
            RetrievalIntent::TopK,
        ));
        assert_eq!(plan.budget().fields().result_limit, 10);
    }
}

#[test]
fn selectivity_classes_choose_only_bounded_declared_paths() {
    let ordinary_classes = [
        SelectivityClass::OneHundredPercent,
        SelectivityClass::TenPercent,
        SelectivityClass::OnePercent,
        SelectivityClass::PointOnePercent,
        SelectivityClass::PointZeroOnePercent,
        SelectivityClass::SingleDocument,
    ];

    for class in ordinary_classes {
        let plan = must_ok(plan_for(
            if class == SelectivityClass::SingleDocument {
                1
            } else {
                25
            },
            class,
            CardinalityFreshness::Fresh,
            EstimateReliability::Verified,
            EstimateHandlingMode::FailClosed,
            StrictPredicateHandlingMode::FailClosed,
            capability(),
            RetrievalIntent::TopK,
        ));
        assert_eq!(plan.path(), SelectedPath::ExactSimdScan);
    }

    let exhaustive = must_ok(plan_for(
        25,
        SelectivityClass::Exhaustive,
        CardinalityFreshness::Fresh,
        EstimateReliability::Verified,
        EstimateHandlingMode::FailClosed,
        StrictPredicateHandlingMode::FailClosed,
        capability(),
        RetrievalIntent::Exhaustive,
    ));
    assert_eq!(exhaustive.path(), SelectedPath::ExhaustiveEnumeration);
}

#[test]
fn stale_and_known_wrong_estimates_fail_closed_or_use_a_named_degraded_profile() {
    let stale_code = plan_for(
        20,
        SelectivityClass::OnePercent,
        CardinalityFreshness::Stale,
        EstimateReliability::Verified,
        EstimateHandlingMode::FailClosed,
        StrictPredicateHandlingMode::FailClosed,
        capability(),
        RetrievalIntent::TopK,
    )
    .err()
    .map(|error| error.diagnostic_code());
    assert_eq!(
        stale_code,
        Some(SearchPlannerDiagnosticCode::StaleCardinalityEstimate)
    );

    let wrong_code = plan_for(
        20,
        SelectivityClass::OnePercent,
        CardinalityFreshness::Fresh,
        EstimateReliability::KnownWrong,
        EstimateHandlingMode::FailClosed,
        StrictPredicateHandlingMode::FailClosed,
        capability(),
        RetrievalIntent::TopK,
    )
    .err()
    .map(|error| error.diagnostic_code());
    assert_eq!(
        wrong_code,
        Some(SearchPlannerDiagnosticCode::UntrustedCardinalityEstimate)
    );

    let degraded = must_ok(plan_for(
        20,
        SelectivityClass::OnePercent,
        CardinalityFreshness::Stale,
        EstimateReliability::Verified,
        EstimateHandlingMode::NamedBoundedDegradedProfile,
        StrictPredicateHandlingMode::FailClosed,
        capability(),
        RetrievalIntent::TopK,
    ));
    assert_eq!(
        degraded.path(),
        SelectedPath::NamedDegradedProfile(NamedDegradedProfile::UntrustedCardinality)
    );
}

#[test]
fn calibrated_large_scope_uses_predicate_aware_diskann3_and_marks_partial() {
    let plan = must_ok(plan_for(
        101,
        SelectivityClass::PointZeroOnePercent,
        CardinalityFreshness::Fresh,
        EstimateReliability::Verified,
        EstimateHandlingMode::FailClosed,
        StrictPredicateHandlingMode::FailClosed,
        capability(),
        RetrievalIntent::TopK,
    ));

    assert_eq!(plan.path(), SelectedPath::PredicateAwareDiskAnn3);
    assert_eq!(
        plan.planned_completeness(),
        PlannedCompleteness::ApproximatePartial
    );
    let profile = match plan.approximate_profile() {
        Some(profile) => profile,
        None => panic!("predicate-aware DiskANN3 plan must publish its bounded profile"),
    };
    assert_eq!(profile.candidate_limit(), 64);
    assert_eq!(profile.beam_width(), 32);
    assert_eq!(profile.probe_count(), 64);
    assert_eq!(plan.full_precision_rescore_budget().candidate_limit(), 32);
}

#[test]
fn unsupported_strict_predicate_is_closed_or_visibly_degraded() {
    let mut unsupported_fields = capability_fields();
    unsupported_fields.supports_pre_rank_predicates = false;
    unsupported_fields.supports_in_traversal_predicates = false;
    let unsupported = must_ok(BackendCapability::new(unsupported_fields));

    let closed_code = plan_for(
        101,
        SelectivityClass::OnePercent,
        CardinalityFreshness::Fresh,
        EstimateReliability::Verified,
        EstimateHandlingMode::FailClosed,
        StrictPredicateHandlingMode::FailClosed,
        unsupported.clone(),
        RetrievalIntent::TopK,
    )
    .err()
    .map(|error| error.diagnostic_code());
    assert_eq!(
        closed_code,
        Some(SearchPlannerDiagnosticCode::StrictPredicateUnsupported)
    );

    let degraded = must_ok(plan_for(
        101,
        SelectivityClass::OnePercent,
        CardinalityFreshness::Fresh,
        EstimateReliability::Verified,
        EstimateHandlingMode::FailClosed,
        StrictPredicateHandlingMode::NamedBoundedDegradedProfile,
        unsupported,
        RetrievalIntent::TopK,
    ));
    assert_eq!(
        degraded.path(),
        SelectedPath::NamedDegradedProfile(NamedDegradedProfile::UnsupportedStrictPredicate)
    );
    assert_eq!(
        degraded.planned_completeness(),
        PlannedCompleteness::DegradedPartial
    );
}

#[test]
fn exhaustive_request_cannot_exceed_its_exact_candidate_budget() {
    let code = plan_for(
        129,
        SelectivityClass::Exhaustive,
        CardinalityFreshness::Fresh,
        EstimateReliability::Verified,
        EstimateHandlingMode::FailClosed,
        StrictPredicateHandlingMode::FailClosed,
        capability(),
        RetrievalIntent::Exhaustive,
    )
    .err()
    .map(|error| error.diagnostic_code());

    assert_eq!(
        code,
        Some(SearchPlannerDiagnosticCode::ExhaustiveBudgetExceeded)
    );
}

#[test]
fn calibration_generation_must_match_the_request_generation() {
    let mismatched_profile = must_ok(SelectivityProfile::new(
        SelectivityClass::OnePercent,
        must_ok(CrossoverThresholds::new(6, 100, 101, 1_000)),
    ));
    let request = must_ok(PlannerRequest::new(
        budget(),
        estimate(
            20,
            CardinalityFreshness::Fresh,
            EstimateReliability::Verified,
        ),
        mismatched_profile,
        capability(),
        generation(),
        RetrievalIntent::TopK,
        EstimateHandlingMode::FailClosed,
        true,
        StrictPredicateHandlingMode::FailClosed,
        4_096,
        VectorMetric::Cosine,
    ));

    let code = SearchPlanner
        .plan_and_validate(request)
        .err()
        .map(|error| error.diagnostic_code());
    assert_eq!(
        code,
        Some(SearchPlannerDiagnosticCode::GenerationBindingInvalid)
    );
}
