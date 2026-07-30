//! Focused tests for the SSD vector benchmark contract (Refs #382 / EVAL-SSD-001).

use crate::ssd_vector_benchmark::{
    all_passing_local_subset_measurements, evaluate_suite_verdict, passing_injected_measurement,
    BackendGateOutcome, BackendId, BackendRole, CacheState, CgroupMemoryMeasurement,
    CgroupMemoryMeasurementFields, ClosedLabel, ComparisonIdentity, ComparisonIdentityFields,
    ConcurrencyLevel, ContentDigest, CorpusIdentity, CorpusIdentityFields, FilterSelectivity,
    GateVerdict, GroundTruthConfigFields, InjectedScenarioMeasurement, LatencyMicros,
    LocalSubsetPlan, OriginalVectorPrecision, QualityMetricKind, QualityMetricObservationFields,
    QualityMetrics, QualityMetricsFields, QualityStage, QueryClass, QueryMatrix, QueryScenario,
    QueryScenarioFields, ResourceMetricsFields, SsdVectorBenchmarkDiagnosticCode,
    SsdVectorBenchmarkError, StorageGrowthPoint, StorageGrowthSeries, StorageGrowthSeriesFields,
    SystemsCatalog, UpdateState, REQUIRED_VECTOR_DIMENSION,
};

#[test]
fn ssd_vector_benchmark_happy_path_local_subset_report() {
    let plan = LocalSubsetPlan::deterministic_default("6a61787", "c0ffeedeadbeef01")
        .expect("local subset plan");
    assert_eq!(plan.schema_version(), 1);
    assert_eq!(
        plan.comparison_identity().dimension(),
        REQUIRED_VECTOR_DIMENSION
    );

    let measurements = all_passing_local_subset_measurements(&plan).expect("passing measurements");
    let report = plan
        .run_with_injected(&measurements)
        .expect("local subset run");

    assert_eq!(report.schema_version(), 1);
    assert!(!report.report_id().is_empty());
    assert_eq!(report.git_revision(), "6a61787");
    assert_eq!(report.verdict(), GateVerdict::Pass);
    assert!(!report.results().is_empty());
    assert!(report.storage_growth().is_some());

    let markdown = report.to_markdown();
    assert!(markdown.contains("verdict: **pass**"));
    assert!(markdown.contains("schema_version: 1"));

    let json = serde_json::to_string(&report).expect("serialize report");
    assert!(json.contains("local-subset"));
}

#[test]
fn ssd_vector_benchmark_dimension_reduction_rejected() {
    let err = ComparisonIdentity::new(ComparisonIdentityFields {
        vectors_digest: "0123456789abcdef0123456789abcdef".to_string(),
        filters_digest: "fedcba9876543210fedcba9876543210".to_string(),
        budgets_digest: "aabbccddeeff00112233445566778899".to_string(),
        qrels_digest: "99aa88bb77cc66dd55ee44ff33221100".to_string(),
        final_scoring_policy: "exact-f32-cosine-full-dim-v1".to_string(),
        dimension: 768,
    })
    .expect_err("dimension reduction forbidden");
    assert_eq!(
        err.diagnostic_code(),
        SsdVectorBenchmarkDiagnosticCode::DimensionReductionForbidden
    );

    let err = CorpusIdentity::new(CorpusIdentityFields {
        scale: crate::ssd_vector_benchmark::CorpusScale::LocalSubsetSynthetic,
        corpus_id: "local-subset-v1".to_string(),
        dataset_digest: "a1b2c3d4e5f60718293a4b5c6d7e8f90".to_string(),
        vector_count: 100,
        source_count: 2,
        ground_truth: GroundTruthConfigFields {
            exact_full_dimensional: true,
            original_precision: OriginalVectorPrecision::F32,
            dimension: 1024,
        },
    })
    .expect_err("corpus dimension reduction forbidden");
    assert_eq!(
        err.diagnostic_code(),
        SsdVectorBenchmarkDiagnosticCode::DimensionReductionForbidden
    );
}

#[test]
fn ssd_vector_benchmark_missing_cold_warm_rejected() {
    let cold_only = QueryScenario::new(QueryScenarioFields {
        scenario_id: "only-cold".to_string(),
        query_class: QueryClass::Semantic,
        filter_selectivity: FilterSelectivity::Full100,
        concurrency: ConcurrencyLevel::One,
        cache_state: CacheState::Cold,
        update_state: UpdateState::ReadOnly,
        query_count: 8,
    })
    .expect("cold scenario");
    let err = QueryMatrix::new(vec![cold_only]).expect_err("warm required");
    assert_eq!(
        err.diagnostic_code(),
        SsdVectorBenchmarkDiagnosticCode::MissingColdWarmCacheState
    );
}

#[test]
fn ssd_vector_benchmark_missing_cgroup_memory_fails_gate() {
    let plan = LocalSubsetPlan::deterministic_default("rev1", "deadbeef01").expect("plan");
    let mut measurements = all_passing_local_subset_measurements(&plan).expect("measurements");
    // Poison one measurement with unmeasured cgroup memory.
    measurements[0].resources.cgroup_memory.measured = false;

    let report = plan
        .run_with_injected(&measurements)
        .expect("run still constructs report");
    // At least one scenario fails; suite should not be clean Pass for all primaries.
    assert!(
        report.results().iter().any(|r| !r.scenario_gate_passed()),
        "missing cgroup measurement must fail scenario gate"
    );

    let unknown = CgroupMemoryMeasurement::unknown();
    assert_eq!(
        unknown
            .require_measured()
            .expect_err("unknown must fail")
            .diagnostic_code(),
        SsdVectorBenchmarkDiagnosticCode::MissingCgroupMemoryMeasurement
    );
}

#[test]
fn ssd_vector_benchmark_unequal_comparison_identity_rejected() {
    let a = ComparisonIdentity::new(ComparisonIdentityFields {
        vectors_digest: "0123456789abcdef0123456789abcdef".to_string(),
        filters_digest: "fedcba9876543210fedcba9876543210".to_string(),
        budgets_digest: "aabbccddeeff00112233445566778899".to_string(),
        qrels_digest: "99aa88bb77cc66dd55ee44ff33221100".to_string(),
        final_scoring_policy: "exact-f32-cosine-full-dim-v1".to_string(),
        dimension: REQUIRED_VECTOR_DIMENSION,
    })
    .expect("identity a");
    let b = ComparisonIdentity::new(ComparisonIdentityFields {
        vectors_digest: "ffffffffffffffffffffffffffffffff".to_string(),
        filters_digest: "fedcba9876543210fedcba9876543210".to_string(),
        budgets_digest: "aabbccddeeff00112233445566778899".to_string(),
        qrels_digest: "99aa88bb77cc66dd55ee44ff33221100".to_string(),
        final_scoring_policy: "exact-f32-cosine-full-dim-v1".to_string(),
        dimension: REQUIRED_VECTOR_DIMENSION,
    })
    .expect("identity b");
    assert_eq!(
        a.require_equal(&b)
            .expect_err("unequal digests")
            .diagnostic_code(),
        SsdVectorBenchmarkDiagnosticCode::UnequalComparisonIdentity
    );
}

#[test]
fn ssd_vector_benchmark_reference_complete_gate_win_forces_reconsideration() {
    let outcomes = vec![
        BackendGateOutcome {
            backend_id: BackendId::Diskann3Standard,
            role: BackendRole::PrimaryCandidate,
            complete_gate_passed: false,
        },
        BackendGateOutcome {
            backend_id: BackendId::QdrantReference,
            role: BackendRole::Reference,
            complete_gate_passed: true,
        },
    ];
    let verdict = evaluate_suite_verdict(&outcomes).expect("suite verdict");
    assert_eq!(verdict, GateVerdict::ArchitectureDecisionMustBeReconsidered);

    // Also via local subset run: make only reference backends pass hard.
    let plan = LocalSubsetPlan::deterministic_default("rev2", "deadbeef02").expect("plan");
    let mut measurements = all_passing_local_subset_measurements(&plan).expect("measurements");
    for m in &mut measurements {
        if m.backend_id.default_role() == BackendRole::PrimaryCandidate {
            // Drop candidate recall below threshold so primary fails.
            for obs in &mut m.quality.observations {
                if obs.stage == QualityStage::Candidate && obs.kind == QualityMetricKind::RecallAt10
                {
                    obs.value = 0.10;
                }
            }
        }
    }
    let report = plan
        .run_with_injected(&measurements)
        .expect("run with reference win");
    // References still pass; primaries fail => reconsideration.
    assert_eq!(
        report.verdict(),
        GateVerdict::ArchitectureDecisionMustBeReconsidered
    );
}

#[test]
fn ssd_vector_benchmark_regression_only_cannot_promote_alone() {
    let outcomes = vec![
        BackendGateOutcome {
            backend_id: BackendId::SqliteScanRegression,
            role: BackendRole::RegressionOnly,
            complete_gate_passed: true,
        },
        BackendGateOutcome {
            backend_id: BackendId::InstantDistanceHnswRegression,
            role: BackendRole::RegressionOnly,
            complete_gate_passed: true,
        },
        BackendGateOutcome {
            backend_id: BackendId::Diskann3Standard,
            role: BackendRole::PrimaryCandidate,
            complete_gate_passed: false,
        },
    ];
    let err = evaluate_suite_verdict(&outcomes).expect_err("regression-only cannot promote");
    assert_eq!(
        err.diagnostic_code(),
        SsdVectorBenchmarkDiagnosticCode::RegressionOnlyCannotPromote
    );

    assert!(!BackendRole::RegressionOnly.can_promote_alone());
    assert!(BackendRole::PrimaryCandidate.can_promote_alone());
    assert!(BackendRole::Reference.can_force_architecture_reconsideration());
    assert!(!BackendRole::ExternalControl.counts_against_verbatim_process_budget());
}

#[test]
fn ssd_vector_benchmark_storage_growth_series_required() {
    let err = StorageGrowthSeries::new(StorageGrowthSeriesFields {
        by_vector_count: vec![StorageGrowthPoint {
            x: 100,
            index_bytes: 1_000,
        }],
        by_source_count: vec![
            StorageGrowthPoint {
                x: 1,
                index_bytes: 1_000,
            },
            StorageGrowthPoint {
                x: 2,
                index_bytes: 2_000,
            },
        ],
    })
    .expect_err("N series needs >=2 points");
    assert_eq!(
        err.diagnostic_code(),
        SsdVectorBenchmarkDiagnosticCode::MissingStorageGrowthSeries
    );

    let err = StorageGrowthSeries::new(StorageGrowthSeriesFields {
        by_vector_count: vec![
            StorageGrowthPoint {
                x: 100,
                index_bytes: 1_000,
            },
            StorageGrowthPoint {
                x: 200,
                index_bytes: 2_000,
            },
        ],
        by_source_count: vec![],
    })
    .expect_err("source series required");
    assert_eq!(
        err.diagnostic_code(),
        SsdVectorBenchmarkDiagnosticCode::MissingStorageGrowthSeries
    );
}

#[test]
fn ssd_vector_benchmark_serde_invalid_payload_rejected() {
    // Dimension reduction via serde.
    let payload = r#"{
        "vectors_digest": "0123456789abcdef0123456789abcdef",
        "filters_digest": "fedcba9876543210fedcba9876543210",
        "budgets_digest": "aabbccddeeff00112233445566778899",
        "qrels_digest": "99aa88bb77cc66dd55ee44ff33221100",
        "final_scoring_policy": "exact-f32-cosine-full-dim-v1",
        "dimension": 512
    }"#;
    let err = serde_json::from_str::<ComparisonIdentity>(payload)
        .expect_err("serde must revalidate dimension");
    let msg = err.to_string();
    assert!(
        msg.contains("dimension_reduction_forbidden") || msg.contains("ssd-vector-benchmark"),
        "unexpected serde error: {msg}"
    );

    // Path-like label rejected.
    let err = ClosedLabel::new("../etc/passwd").expect_err("path-like label");
    assert_eq!(
        err.diagnostic_code(),
        SsdVectorBenchmarkDiagnosticCode::InvalidIdentity
    );

    // Empty digest rejected.
    let err = ContentDigest::new("").expect_err("empty digest");
    assert_eq!(
        err.diagnostic_code(),
        SsdVectorBenchmarkDiagnosticCode::InvalidIdentity
    );

    // Candidate-only quality metrics rejected.
    let err = QualityMetrics::new(vec![
        crate::ssd_vector_benchmark::QualityMetricObservation::new(
            QualityMetricObservationFields {
                stage: QualityStage::Candidate,
                kind: QualityMetricKind::RecallAt10,
                value: 0.9,
            },
        )
        .expect("obs"),
    ])
    .expect_err("final stage required");
    assert_eq!(
        err.diagnostic_code(),
        SsdVectorBenchmarkDiagnosticCode::CandidateFinalQualityMustBeSeparate
    );
}

#[test]
fn ssd_vector_benchmark_error_display_is_code_only() {
    for code in SsdVectorBenchmarkDiagnosticCode::ALL {
        let err = SsdVectorBenchmarkError::contract(code);
        let display = err.to_string();
        assert_eq!(display, format!("ssd-vector-benchmark.{}", code.as_str()));
        let debug = format!("{err:?}");
        assert_eq!(debug, format!("SsdVectorBenchmarkError({})", code.as_str()));
        // No path or free-form payload leakage patterns.
        assert!(!display.contains('/'));
        assert!(!debug.contains("home"));
    }
}

#[test]
fn ssd_vector_benchmark_required_backends_catalog() {
    let catalog = SystemsCatalog::required_defaults().expect("required catalog");
    let ids: Vec<_> = catalog.systems().iter().map(|s| s.backend_id()).collect();
    assert!(ids.contains(&BackendId::Diskann3Standard));
    assert!(ids.contains(&BackendId::Diskann3AisaqColocatedPerformance));
    assert!(ids.contains(&BackendId::Diskann3AisaqColocatedScale));
    assert!(ids.contains(&BackendId::ExactFullDimensionalFlatScan));
    assert!(ids.contains(&BackendId::QdrantReference));
    assert!(ids.contains(&BackendId::LancedbIvfRq));
    assert!(ids.contains(&BackendId::LancedbIvfPq));
    assert!(ids.contains(&BackendId::SqliteScanRegression));
    assert!(ids.contains(&BackendId::InstantDistanceHnswRegression));
    assert!(!ids.contains(&BackendId::UsearchHnswControl));
    assert!(!ids.contains(&BackendId::MilvusAisaqControl));
}

#[test]
fn ssd_vector_benchmark_exact_ground_truth_required() {
    let err = crate::ssd_vector_benchmark::GroundTruthConfig::new(GroundTruthConfigFields {
        exact_full_dimensional: false,
        original_precision: OriginalVectorPrecision::F32,
        dimension: REQUIRED_VECTOR_DIMENSION,
    })
    .expect_err("exact ground truth required");
    assert_eq!(
        err.diagnostic_code(),
        SsdVectorBenchmarkDiagnosticCode::ExactGroundTruthRequired
    );
}

#[test]
fn ssd_vector_benchmark_missing_measurement_fails_run() {
    let plan = LocalSubsetPlan::deterministic_default("rev3", "deadbeef03").expect("plan");
    // Empty measurements => missing cells.
    let err = plan
        .run_with_injected(&[])
        .expect_err("missing measurements");
    assert_eq!(
        err.diagnostic_code(),
        SsdVectorBenchmarkDiagnosticCode::MissingMeasurement
    );
}

#[test]
fn ssd_vector_benchmark_cgroup_fields_construct() {
    let measured = CgroupMemoryMeasurement::new(CgroupMemoryMeasurementFields {
        memory_current_bytes: 50,
        memory_high_bytes: 100,
        memory_max_bytes: 200,
        peak_bytes: 80,
        measured: true,
    })
    .expect("measured cgroup");
    assert!(measured.measured());
    assert_eq!(measured.memory_current_bytes(), 50);

    let err = CgroupMemoryMeasurement::new(CgroupMemoryMeasurementFields {
        memory_current_bytes: 50,
        memory_high_bytes: 300,
        memory_max_bytes: 200,
        peak_bytes: 80,
        measured: true,
    })
    .expect_err("high > max invalid");
    assert_eq!(
        err.diagnostic_code(),
        SsdVectorBenchmarkDiagnosticCode::InvalidBounds
    );
}

#[test]
fn ssd_vector_benchmark_passing_helper_builds_valid_injection() {
    let m = passing_injected_measurement(
        BackendId::Diskann3Standard,
        "local-semantic-broad-cold",
        CacheState::Cold,
    )
    .expect("helper");
    assert_eq!(m.backend_id, BackendId::Diskann3Standard);
    assert_eq!(m.cache_state, CacheState::Cold);
    // Round-trip quality fields.
    let _ = InjectedScenarioMeasurement {
        backend_id: m.backend_id,
        scenario_id: m.scenario_id.clone(),
        cache_state: m.cache_state,
        quality: m.quality.clone(),
        resources: ResourceMetricsFields {
            latency: LatencyMicros::new(1, 2, 3).expect("lat"),
            throughput_qps_milli: 1,
            cgroup_memory: m.resources.cgroup_memory,
            major_faults: None,
            minor_faults: None,
            ssd_bytes_per_query: None,
            ssd_ops_per_query: None,
            index_bytes: None,
        },
    };
    let _ = QualityMetricsFields {
        observations: m.quality.observations.clone(),
    };
}
