use crate::diskann3::PublicationGeneration;
use crate::lancedb_backend::{
    AdaptiveProbePlan, BackendSelection, CandidateLossReport, FilterClause, FilterStrictness,
    LanceDbBackendDiagnosticCode, LanceDbCapabilities, LanceDbCapabilityFields,
    LanceDbCollectionIdentity, LanceDbCollectionSchema, LanceDbFilterContract, LanceDbHitRef,
    LanceDbIndexProfile, LanceDbLexicalPolicy, LanceDbLifecycleState, LanceDbLifecycleTransition,
    LanceDbOperationBudget, LanceDbQualityPlan, LanceDbScalarIndexKind, LanceDbScalarIndexPlan,
    LanceDbScalarIndexRequirement, LanceDbSchemaFields, LanceDbSearchPolicy, LanceDbSearchRequest,
    LexicalConformanceFlag, LexicalOwnership, TableName,
};
use crate::search_planner::{SearchBudget, SearchBudgetFields};
use crate::types::EmbeddingProfileId;

fn budget() -> SearchBudget {
    SearchBudget::new(SearchBudgetFields {
        result_limit: 5,
        dense_candidate_limit: 10,
        lexical_candidate_limit: 10,
        exact_candidate_limit: 10,
        graph_candidate_limit: 10,
        fused_pool_limit: 10,
        rerank_candidate_limit: 10,
        full_precision_rescore_limit: 5,
        hydration_limit: 5,
        max_ssd_pages: 10,
        max_bytes_read: 1_024,
        max_cpu_micros: 1_024,
        max_work_units: 1_024,
        max_wall_time_micros: 1_024,
        max_concurrent_stages: 1,
        max_stage_attempts: 1,
        debug_record_limit: 5,
    })
    .expect("valid test budget")
}

fn identity(generation: u64) -> LanceDbCollectionIdentity {
    LanceDbCollectionIdentity::new(
        TableName::new("enterprise_vectors").expect("table"),
        EmbeddingProfileId::new("default").expect("profile"),
        PublicationGeneration::new(generation).expect("generation"),
        "0".repeat(64),
    )
    .expect("identity")
}

fn schema() -> LanceDbCollectionSchema {
    LanceDbCollectionSchema::new(
        identity(7),
        LanceDbSchemaFields {
            dimension: LanceDbCollectionSchema::DIMENSION,
            original_vectors_f32_retained: true,
            full_dimension_required: true,
        },
    )
    .expect("schema")
}

fn scalar_indexes() -> LanceDbScalarIndexPlan {
    LanceDbScalarIndexPlan::new(vec![
        LanceDbScalarIndexRequirement::new("source", LanceDbScalarIndexKind::BTree)
            .expect("source"),
        LanceDbScalarIndexRequirement::new("collection", LanceDbScalarIndexKind::BTree)
            .expect("collection"),
        LanceDbScalarIndexRequirement::new("tenant", LanceDbScalarIndexKind::BTree)
            .expect("tenant"),
        LanceDbScalarIndexRequirement::new("acl", LanceDbScalarIndexKind::LabelList).expect("acl"),
        LanceDbScalarIndexRequirement::new("lifecycle", LanceDbScalarIndexKind::Bitmap)
            .expect("lifecycle"),
        LanceDbScalarIndexRequirement::new("timestamp_micros", LanceDbScalarIndexKind::BTree)
            .expect("time"),
    ])
    .expect("scalar indexes")
}

fn operation_budget() -> LanceDbOperationBudget {
    LanceDbOperationBudget::new(budget(), budget()).expect("operation budget")
}

#[test]
fn lancedb_backend_happy_path_is_4096_dimensional_and_lancedb_primary() {
    let filter = LanceDbFilterContract::new(
        FilterStrictness::StrictNativeOrFailClosed,
        vec![FilterClause::Tenant {
            value: "tenant-a".into(),
        }],
        scalar_indexes(),
        10_000,
    )
    .expect("filter");
    let request = LanceDbSearchRequest::new(
        schema(),
        LanceDbIndexProfile::IvfRq,
        LanceDbSearchPolicy::lancedb_primary(identity(7), budget()).expect("policy"),
        filter,
        AdaptiveProbePlan::new(2, 16).expect("probes"),
        LanceDbQualityPlan::new(2, true, true).expect("quality"),
        operation_budget(),
        5,
    )
    .expect("request");
    assert_eq!(request.schema().dimension(), 4_096);
    assert_eq!(request.profile(), LanceDbIndexProfile::IvfRq);
    assert_eq!(
        request.policy().selection(),
        BackendSelection::LanceDbPrimary
    );
}

#[test]
fn lancedb_backend_rejects_dimension_reduction_or_missing_originals() {
    let error = LanceDbCollectionSchema::new(
        identity(7),
        LanceDbSchemaFields {
            dimension: 3_072,
            original_vectors_f32_retained: true,
            full_dimension_required: true,
        },
    )
    .expect_err("dimension reduction fails closed");
    assert_eq!(
        error.diagnostic_code(),
        LanceDbBackendDiagnosticCode::VectorDimensionMismatch
    );

    let error = LanceDbCollectionSchema::new(
        identity(7),
        LanceDbSchemaFields {
            dimension: 4_096,
            original_vectors_f32_retained: false,
            full_dimension_required: true,
        },
    )
    .expect_err("original vectors are required");
    assert_eq!(
        error.diagnostic_code(),
        LanceDbBackendDiagnosticCode::InvalidSchema
    );
}

#[test]
fn lancedb_backend_ivf_pq_subvectors_have_closed_bounds() {
    for profile in [
        LanceDbIndexProfile::ivf_pq(0),
        LanceDbIndexProfile::ivf_pq(LanceDbIndexProfile::MAX_PQ_SUB_VECTORS + 1),
    ] {
        let error = profile.expect_err("PQ bounds are enforced");
        assert_eq!(
            error.diagnostic_code(),
            LanceDbBackendDiagnosticCode::InvalidIndexProfile
        );
    }
    assert_eq!(
        LanceDbIndexProfile::ivf_pq(64).expect("valid PQ profile"),
        LanceDbIndexProfile::IvfPq {
            num_sub_vectors: 64
        }
    );
}

#[test]
fn lancedb_backend_index_profile_serde_revalidates_pq_bounds() {
    let decoded: Result<LanceDbIndexProfile, _> = serde_json::from_value(serde_json::json!({
        "ivf_pq": { "num_sub_vectors": 0 }
    }));
    assert!(
        decoded.is_err(),
        "serde must use the validated PQ constructor"
    );
}

#[test]
fn lancedb_backend_profiles_keep_control_and_ground_truth_explicit() {
    assert!(LanceDbIndexProfile::IvfRq.is_quantized_candidate_generation());
    assert!(LanceDbIndexProfile::IvfHnswFlat.is_high_recall_control());
    assert!(LanceDbIndexProfile::BypassExactScan.is_exact_scan());
}

#[test]
fn lancedb_backend_search_requests_bind_each_index_profile() {
    for profile in [
        LanceDbIndexProfile::IvfRq,
        LanceDbIndexProfile::ivf_pq(64).expect("valid PQ profile"),
        LanceDbIndexProfile::IvfHnswFlat,
        LanceDbIndexProfile::IvfHnswSq,
        LanceDbIndexProfile::BypassExactScan,
    ] {
        let filter = LanceDbFilterContract::new(
            FilterStrictness::StrictNativeOrFailClosed,
            vec![FilterClause::Tenant {
                value: "tenant-a".into(),
            }],
            scalar_indexes(),
            10_000,
        )
        .expect("filter");
        let request = LanceDbSearchRequest::new(
            schema(),
            profile,
            LanceDbSearchPolicy::lancedb_primary(identity(7), budget()).expect("policy"),
            filter,
            AdaptiveProbePlan::new(2, 16).expect("probes"),
            LanceDbQualityPlan::new(2, true, true).expect("quality"),
            operation_budget(),
            5,
        )
        .expect("profile must remain bound to a valid request");
        assert_eq!(request.profile(), profile);
    }
}

#[test]
fn lancedb_backend_search_request_revalidates_directly_constructed_pq_profile() {
    let filter = LanceDbFilterContract::new(
        FilterStrictness::StrictNativeOrFailClosed,
        vec![FilterClause::Tenant {
            value: "tenant-a".into(),
        }],
        scalar_indexes(),
        10_000,
    )
    .expect("filter");
    let error = LanceDbSearchRequest::new(
        schema(),
        LanceDbIndexProfile::IvfPq { num_sub_vectors: 0 },
        LanceDbSearchPolicy::lancedb_primary(identity(7), budget()).expect("policy"),
        filter,
        AdaptiveProbePlan::new(2, 16).expect("probes"),
        LanceDbQualityPlan::new(2, true, true).expect("quality"),
        operation_budget(),
        5,
    )
    .expect_err("request must reject a profile that bypassed its constructor");
    assert_eq!(
        error.diagnostic_code(),
        LanceDbBackendDiagnosticCode::InvalidIndexProfile
    );
}

#[test]
fn lancedb_backend_adaptive_probes_validate_bounds_and_change_with_selectivity() {
    for invalid in [(0, 2), (4, 2), (1, AdaptiveProbePlan::HARD_MAX_NPROBES + 1)] {
        let error = AdaptiveProbePlan::new(invalid.0, invalid.1).expect_err("invalid probes");
        assert_eq!(
            error.diagnostic_code(),
            LanceDbBackendDiagnosticCode::InvalidProbePlan
        );
    }
    let plan = AdaptiveProbePlan::new(2, 16).expect("adaptive plan");
    assert!(plan.nprobes_for_selectivity(10_000) < plan.nprobes_for_selectivity(900_000));
}

#[test]
fn lancedb_backend_strict_filter_requires_matching_scalar_index() {
    let indexes = LanceDbScalarIndexPlan::new(vec![
        LanceDbScalarIndexRequirement::new("tenant", LanceDbScalarIndexKind::BTree)
            .expect("tenant"),
        LanceDbScalarIndexRequirement::new("acl", LanceDbScalarIndexKind::LabelList).expect("acl"),
        LanceDbScalarIndexRequirement::new("lifecycle", LanceDbScalarIndexKind::Bitmap)
            .expect("lifecycle"),
    ])
    .expect("partial scalar indexes");
    let error = LanceDbFilterContract::new(
        FilterStrictness::StrictNativeOrFailClosed,
        vec![FilterClause::Source {
            value: "source-a".into(),
        }],
        indexes,
        50_000,
    )
    .expect_err("unbound source predicate fails closed");
    assert_eq!(
        error.diagnostic_code(),
        LanceDbBackendDiagnosticCode::StrictFilterUnbound
    );
}

#[test]
fn lancedb_backend_time_filter_requires_btree_binding() {
    let error = LanceDbFilterContract::new(
        FilterStrictness::StrictNativeOrFailClosed,
        vec![FilterClause::TimestampMicros { min: 1, max: 2 }],
        scalar_indexes(),
        50_000,
    );
    assert!(error.is_ok(), "timestamp binds to its required BTree index");
}

#[test]
fn lancedb_backend_quality_requires_refinement_and_original_rescoring() {
    let error = LanceDbQualityPlan::new(0, true, true).expect_err("refine factor must be positive");
    assert_eq!(
        error.diagnostic_code(),
        LanceDbBackendDiagnosticCode::InvalidQualityPlan
    );
    let error = LanceDbQualityPlan::new(2, false, true).expect_err("original vectors required");
    assert_eq!(
        error.diagnostic_code(),
        LanceDbBackendDiagnosticCode::InvalidQualityPlan
    );
}

#[test]
fn lancedb_backend_candidate_loss_is_first_class_and_bounded() {
    let report = CandidateLossReport::new(100, 80, 5).expect("bounded loss report");
    assert_eq!(report.omitted_ground_truth_neighbors(), 5);
    let error = CandidateLossReport::new(10, 11, 0).expect_err("returned cannot exceed generated");
    assert_eq!(
        error.diagnostic_code(),
        LanceDbBackendDiagnosticCode::InvalidCandidateLossReport
    );
}

#[test]
fn lancedb_backend_rejects_stale_or_mixed_generation_hydration() {
    let stale = LanceDbHitRef::new(
        "chunk-1",
        "default",
        PublicationGeneration::new(6).expect("gen"),
    )
    .expect("hit");
    let error = identity(7)
        .validate_hydration(&stale)
        .expect_err("stale hit");
    assert_eq!(
        error.diagnostic_code(),
        LanceDbBackendDiagnosticCode::StaleGenerationHydration
    );

    let wrong = LanceDbHitRef::new(
        "chunk-1",
        "other",
        PublicationGeneration::new(8).expect("gen"),
    )
    .expect("hit");
    let error = identity(7)
        .validate_hydration(&wrong)
        .expect_err("wrong binding");
    assert_eq!(
        error.diagnostic_code(),
        LanceDbBackendDiagnosticCode::WrongGenerationHydration
    );
}

#[test]
fn lancedb_backend_lifecycle_rejects_invalid_order_and_accepts_promotion_path() {
    let error = LanceDbLifecycleTransition::new(
        identity(7),
        LanceDbLifecycleState::Staged,
        LanceDbLifecycleState::Promoted,
    )
    .expect_err("promotion requires optimize and validation");
    assert_eq!(
        error.diagnostic_code(),
        LanceDbBackendDiagnosticCode::InvalidLifecycleTransition
    );
    for (from, to) in [
        (
            LanceDbLifecycleState::Staged,
            LanceDbLifecycleState::Optimized,
        ),
        (
            LanceDbLifecycleState::Optimized,
            LanceDbLifecycleState::Validated,
        ),
        (
            LanceDbLifecycleState::Validated,
            LanceDbLifecycleState::Promoted,
        ),
    ] {
        LanceDbLifecycleTransition::new(identity(7), from, to).expect("ordered transition");
    }
}

#[test]
fn lancedb_backend_lifecycle_exposes_delete_compaction_and_crash_recovery_hooks() {
    for state in [
        LanceDbLifecycleState::Deleted,
        LanceDbLifecycleState::Compacted,
        LanceDbLifecycleState::CrashRecovered,
    ] {
        assert!(state.is_generation_bound_hook());
    }
}

#[test]
fn lancedb_backend_lexical_is_noncanonical_without_issue_380_conformance() {
    let policy = LanceDbLexicalPolicy::new(
        LexicalOwnership::TantivyPrimary,
        LexicalConformanceFlag::NotClaimed,
        true,
    )
    .expect("comparison-only FTS policy");
    let error = policy
        .claim_lancedb_as_canonical()
        .expect_err("conformance is required");
    assert_eq!(
        error.diagnostic_code(),
        LanceDbBackendDiagnosticCode::LexicalConformanceRequired
    );
}

#[test]
fn lancedb_backend_capabilities_require_ivf_scalar_adaptive_and_refine_support() {
    let error = LanceDbCapabilities::new(LanceDbCapabilityFields {
        supports_ivf_rq: true,
        supports_ivf_pq: true,
        supports_hnsw_control: true,
        supports_bypass_exact_scan: true,
        supports_scalar_prefilter: false,
        supports_adaptive_nprobes: true,
        supports_original_vector_rescore: true,
        supports_generation_publication: true,
        supports_optimize_reindex: true,
    })
    .expect_err("scalar prefilter is required");
    assert_eq!(
        error.diagnostic_code(),
        LanceDbBackendDiagnosticCode::InvalidCapabilities
    );
}

#[test]
fn lancedb_backend_serde_revalidates_schema_and_filter_derivations() {
    let encoded = serde_json::to_value(schema()).expect("schema serializes");
    let decoded: LanceDbCollectionSchema =
        serde_json::from_value(encoded).expect("schema validates");
    assert_eq!(decoded, schema());

    let filter = LanceDbFilterContract::new(
        FilterStrictness::BestEffort,
        vec![FilterClause::Source {
            value: "source-a".into(),
        }],
        scalar_indexes(),
        10_000,
    )
    .expect("filter");
    let mut encoded = serde_json::to_value(filter).expect("filter serializes");
    encoded["native_support"] = serde_json::Value::Bool(false);
    let decoded: LanceDbFilterContract = serde_json::from_value(encoded).expect("revalidates");
    assert!(decoded.native_support());
}

#[test]
fn lancedb_backend_diagnostics_are_code_only() {
    let error = AdaptiveProbePlan::new(0, 1).expect_err("invalid");
    assert_eq!(error.to_string(), "lancedb-backend.invalid_probe_plan");
    assert_eq!(
        format!("{error:?}"),
        "LanceDbBackendError(invalid_probe_plan)"
    );
}

#[test]
fn lancedb_backend_serde_rejects_invalid_probe_quality_and_candidate_loss_contracts() {
    for wire in [
        serde_json::json!({ "minimum_nprobes": 0, "maximum_nprobes": 2 }),
        serde_json::json!({ "minimum_nprobes": 4, "maximum_nprobes": 2 }),
        serde_json::json!({ "minimum_nprobes": 2, "maximum_nprobes": 2 }),
    ] {
        let error = serde_json::from_value::<AdaptiveProbePlan>(wire)
            .expect_err("invalid probe wire contract must fail closed");
        assert_eq!(error.to_string(), "lancedb-backend.invalid_probe_plan");
    }

    for wire in [
        serde_json::json!({
            "refine_factor": 0,
            "original_vectors_f32_retained": true,
            "full_precision_rescore_required": true,
        }),
        serde_json::json!({
            "refine_factor": 2,
            "original_vectors_f32_retained": false,
            "full_precision_rescore_required": true,
        }),
    ] {
        let error = serde_json::from_value::<LanceDbQualityPlan>(wire)
            .expect_err("invalid quality wire contract must fail closed");
        assert_eq!(error.to_string(), "lancedb-backend.invalid_quality_plan");
    }

    for wire in [
        serde_json::json!({
            "generated_candidates": 0,
            "rescored_candidates": 0,
            "omitted_ground_truth_neighbors": 0,
        }),
        serde_json::json!({
            "generated_candidates": 10,
            "rescored_candidates": 11,
            "omitted_ground_truth_neighbors": 0,
        }),
    ] {
        let error = serde_json::from_value::<CandidateLossReport>(wire)
            .expect_err("invalid candidate-loss wire contract must fail closed");
        assert_eq!(
            error.to_string(),
            "lancedb-backend.invalid_candidate_loss_report"
        );
    }
}

#[test]
fn lancedb_backend_contract_keeps_the_sealed_marker_private_and_has_no_live_dependency() {
    let source = include_str!("lancedb_backend/contract.rs");
    assert!(source.contains("mod sealed {"));
    assert!(!source.contains("pub use sealed"));
    assert!(!source.contains("lancedb::"));
}
