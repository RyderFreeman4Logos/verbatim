use crate::diskann3::PublicationGeneration;
use crate::qdrant_backend::{
    BackpressureMarker, CollectionName, ConfigDigest, FilterClause, FilterStrictness,
    ForbiddenLocalPreSearch, GrpcPathRequirements, HydrationRequest, LexicalConformanceFlag,
    LexicalOwnership, LocalDenseParticipation, NamedVectorSpaceId, PayloadIndexKind,
    PayloadIndexPlan, PayloadIndexRequirement, QdrantBackendDiagnosticCode, QdrantCapabilities,
    QdrantCapabilityFields, QdrantCollectionIdentity, QdrantCollectionSchema, QdrantFilterContract,
    QdrantLexicalPolicy, QdrantOperationBudget, QdrantPointRef, QdrantQuerySurface,
    QdrantSchemaFields, QdrantSearchPolicy, QdrantSearchRequest, QdrantTransport,
    QdrantVectorMetric, QdrantVectorNormalization, QuantizationProfile, TypedQdrantFailure,
};
use crate::search_planner::{SearchBudget, SearchBudgetFields};
use crate::types::EmbeddingProfileId;

fn budget(attempts: u16) -> SearchBudget {
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
        max_stage_attempts: attempts,
        debug_record_limit: 5,
    })
    .expect("valid test budget")
}

fn identity(generation: u64) -> QdrantCollectionIdentity {
    QdrantCollectionIdentity::new(
        CollectionName::new("enterprise_vectors").expect("collection"),
        NamedVectorSpaceId::new("text_default").expect("named vector"),
        EmbeddingProfileId::new("default").expect("profile"),
        PublicationGeneration::new(generation).expect("generation"),
        ConfigDigest::new("0".repeat(64)).expect("digest"),
    )
    .expect("identity")
}

fn schema() -> QdrantCollectionSchema {
    QdrantCollectionSchema::new(
        identity(7),
        QdrantSchemaFields {
            dimension: QdrantCollectionSchema::DIMENSION,
            metric: QdrantVectorMetric::Cosine,
            normalization: QdrantVectorNormalization::UnitL2,
            quantization: QuantizationProfile::Scalar,
            hnsw_on_disk: true,
            vectors_on_disk: true,
            payload_on_disk: false,
            requires_payload_schema: true,
        },
    )
    .expect("schema")
}

fn payload_indexes() -> PayloadIndexPlan {
    PayloadIndexPlan::new(vec![
        PayloadIndexRequirement::new("tenant", PayloadIndexKind::Keyword).expect("tenant"),
        PayloadIndexRequirement::new("acl", PayloadIndexKind::Acl).expect("acl"),
        PayloadIndexRequirement::new("lifecycle", PayloadIndexKind::Lifecycle).expect("lifecycle"),
    ])
    .expect("payload indexes")
}

fn operation_budget(attempts: u16) -> QdrantOperationBudget {
    let budget = budget(attempts);
    QdrantOperationBudget::new(budget, budget, 1, 1_000, BackpressureMarker::None)
        .expect("operation budget")
}

#[test]
fn qdrant_backend_happy_path_constructs_named_vector_primary_request() {
    let policy = QdrantSearchPolicy::qdrant_primary_only(budget(1)).expect("primary policy");
    let filter = QdrantFilterContract::new(
        FilterStrictness::StrictNativeOrFailClosed,
        vec![
            FilterClause::Tenant {
                value: "tenant-a".into(),
            },
            FilterClause::Acl {
                value: "group-a".into(),
            },
            FilterClause::Lifecycle {
                value: "active".into(),
            },
        ],
        payload_indexes(),
    )
    .expect("native strict filter");
    let request = QdrantSearchRequest::new(schema(), policy, filter, operation_budget(1), 5)
        .expect("valid Qdrant primary request");
    assert_eq!(request.schema().dimension(), 4_096);
    assert_eq!(
        request.schema().identity().named_vector().as_str(),
        "text_default"
    );
    assert!(request.policy().is_qdrant_primary());
    assert!(request.filter().native_support());
}

#[test]
fn qdrant_backend_unconditional_local_pre_search_is_rejected_and_not_compliant() {
    let error = QdrantSearchPolicy::reject_unconditional_local_pre_search(
        ForbiddenLocalPreSearch::UnconditionalLocalDensePreSearch,
    )
    .expect_err("historical local pre-search is forbidden");
    assert_eq!(
        error.diagnostic_code(),
        QdrantBackendDiagnosticCode::UnconditionalLocalPreSearchForbidden
    );
}

#[test]
fn qdrant_backend_primary_only_policy_cannot_authorize_fallback() {
    let policy = QdrantSearchPolicy::qdrant_primary_only(budget(2)).expect("primary policy");
    let receipt = policy
        .record_typed_qdrant_failure(TypedQdrantFailure::TransportUnavailable, budget(1))
        .expect("only a primary policy can mint a receipt");
    let error = policy
        .authorize_local_fallback(&receipt)
        .expect_err("primary-only policy must not authorize fallback directly");
    assert_eq!(
        error.diagnostic_code(),
        QdrantBackendDiagnosticCode::FallbackWithoutTypedFailure
    );
}

#[test]
fn qdrant_backend_fallback_requires_a_typed_failure_receipt_and_remaining_budget() {
    let primary = QdrantSearchPolicy::qdrant_primary_only(budget(2)).expect("primary policy");
    let receipt = primary
        .record_typed_qdrant_failure(TypedQdrantFailure::TransportUnavailable, budget(1))
        .expect("typed failure mints a sealed receipt");
    let policy = primary
        .enable_fallback_after_receipt(&receipt)
        .expect("receipt authorizes the fallback policy");
    policy
        .authorize_local_fallback(&receipt)
        .expect("remaining bounded budget admits fallback");
    assert_eq!(
        policy.local_dense(),
        LocalDenseParticipation::FallbackAfterTypedFailure
    );
}

#[test]
fn qdrant_backend_receipt_cannot_authorize_another_primary_policy() {
    let failed_primary =
        QdrantSearchPolicy::qdrant_primary_only(budget(2)).expect("failed primary policy");
    let receipt = failed_primary
        .record_typed_qdrant_failure(TypedQdrantFailure::TransportUnavailable, budget(1))
        .expect("failed primary mints receipt");
    let unrelated_primary =
        QdrantSearchPolicy::qdrant_primary_only(budget(2)).expect("unrelated primary policy");
    let error = unrelated_primary
        .enable_fallback_after_receipt(&receipt)
        .expect_err("receipt is bound to the primary policy that recorded the failure");
    assert_eq!(
        error.diagnostic_code(),
        QdrantBackendDiagnosticCode::FallbackWithoutTypedFailure
    );
}

#[test]
fn qdrant_backend_exhausted_fallback_budget_fails_closed() {
    let policy = QdrantSearchPolicy::qdrant_primary_only(budget(1)).expect("primary policy");
    let error = policy
        .record_typed_qdrant_failure(TypedQdrantFailure::DeadlineExceeded, budget(1))
        .expect_err("one total attempt cannot include both primary and fallback");
    assert_eq!(
        error.diagnostic_code(),
        QdrantBackendDiagnosticCode::FallbackBudgetExhausted
    );
}

#[test]
fn qdrant_backend_rejects_non_4096_dimension() {
    let error = QdrantCollectionSchema::new(
        identity(7),
        QdrantSchemaFields {
            dimension: 3_072,
            metric: QdrantVectorMetric::Cosine,
            normalization: QdrantVectorNormalization::UnitL2,
            quantization: QuantizationProfile::None,
            hnsw_on_disk: true,
            vectors_on_disk: true,
            payload_on_disk: false,
            requires_payload_schema: true,
        },
    )
    .expect_err("dimension mismatch must fail closed");
    assert_eq!(
        error.diagnostic_code(),
        QdrantBackendDiagnosticCode::VectorDimensionMismatch
    );
}

#[test]
fn qdrant_backend_rejects_stale_generation_hydration() {
    let point = QdrantPointRef::new(
        "chunk-1",
        PublicationGeneration::new(6).expect("generation"),
        "default",
    )
    .expect("point");
    let error =
        HydrationRequest::new(identity(7), vec![point]).expect_err("stale points cannot hydrate");
    assert_eq!(
        error.diagnostic_code(),
        QdrantBackendDiagnosticCode::StaleGenerationHydration
    );
}

#[test]
fn qdrant_backend_rejects_wrong_generation_or_profile_hydration() {
    let point = QdrantPointRef::new(
        "chunk-1",
        PublicationGeneration::new(8).expect("generation"),
        "other-profile",
    )
    .expect("point");
    let error =
        HydrationRequest::new(identity(7), vec![point]).expect_err("wrong identity cannot hydrate");
    assert_eq!(
        error.diagnostic_code(),
        QdrantBackendDiagnosticCode::WrongGenerationHydration
    );
}

#[test]
fn qdrant_backend_strict_source_or_collection_requires_matching_index_field() {
    for clause in [
        FilterClause::Source {
            value: "source-a".into(),
        },
        FilterClause::Collection {
            value: "collection-a".into(),
        },
    ] {
        let error = QdrantFilterContract::new(
            FilterStrictness::StrictNativeOrFailClosed,
            vec![clause],
            payload_indexes(),
        )
        .expect_err("unrelated tenant, ACL, and lifecycle indexes do not cover the clause");
        assert_eq!(
            error.diagnostic_code(),
            QdrantBackendDiagnosticCode::StrictFilterUnsupported
        );
    }
}

#[test]
fn qdrant_backend_range_filter_requires_the_matching_range_index() {
    let mismatched_indexes = PayloadIndexPlan::new(vec![
        PayloadIndexRequirement::new("tenant", PayloadIndexKind::Keyword).expect("tenant"),
        PayloadIndexRequirement::new("acl", PayloadIndexKind::Acl).expect("ACL"),
        PayloadIndexRequirement::new("lifecycle", PayloadIndexKind::Lifecycle).expect("lifecycle"),
        PayloadIndexRequirement::new("published_at", PayloadIndexKind::FloatRange)
            .expect("mismatched range"),
    ])
    .expect("valid unrelated indexes");
    let clause = FilterClause::IntegerRange {
        field: "published_at".into(),
        min: 1,
        max: 2,
    };
    let error = QdrantFilterContract::new(
        FilterStrictness::StrictNativeOrFailClosed,
        vec![clause.clone()],
        mismatched_indexes,
    )
    .expect_err("a float range index cannot cover an integer range clause");
    assert_eq!(
        error.diagnostic_code(),
        QdrantBackendDiagnosticCode::StrictFilterUnsupported
    );

    let matching_indexes = PayloadIndexPlan::new(vec![
        PayloadIndexRequirement::new("tenant", PayloadIndexKind::Keyword).expect("tenant"),
        PayloadIndexRequirement::new("acl", PayloadIndexKind::Acl).expect("ACL"),
        PayloadIndexRequirement::new("lifecycle", PayloadIndexKind::Lifecycle).expect("lifecycle"),
        PayloadIndexRequirement::new("published_at", PayloadIndexKind::IntegerRange)
            .expect("matching range"),
    ])
    .expect("valid matching indexes");
    let contract = QdrantFilterContract::new(
        FilterStrictness::StrictNativeOrFailClosed,
        vec![clause],
        matching_indexes,
    )
    .expect("matching range index supports strict native filtering");
    assert!(contract.native_support());
}

#[test]
fn qdrant_backend_filter_serde_recomputes_native_support_from_indexes() {
    let contract = QdrantFilterContract::new(
        FilterStrictness::BestEffort,
        vec![FilterClause::Source {
            value: "source-a".into(),
        }],
        payload_indexes(),
    )
    .expect("best-effort filter may be unsupported natively");
    let mut encoded = serde_json::to_value(&contract).expect("serializes");
    encoded["native_support"] = serde_json::Value::Bool(true);
    let decoded: QdrantFilterContract =
        serde_json::from_value(encoded).expect("revalidates through the constructor");
    assert!(!decoded.native_support());
}

#[test]
fn qdrant_backend_payload_index_plan_requires_keyword_acl_and_lifecycle() {
    let error = PayloadIndexPlan::new(vec![
        PayloadIndexRequirement::new("tenant", PayloadIndexKind::Keyword).expect("tenant"),
        PayloadIndexRequirement::new("acl", PayloadIndexKind::Acl).expect("acl"),
    ])
    .expect_err("lifecycle is required");
    assert_eq!(
        error.diagnostic_code(),
        QdrantBackendDiagnosticCode::InvalidPayloadIndexPlan
    );
}

#[test]
fn qdrant_backend_capability_discovery_is_closed_and_requires_query_api() {
    let error = QdrantCapabilities::new(QdrantCapabilityFields {
        supports_query_api: false,
        supports_named_vectors: true,
        supports_multivector: true,
        supports_quantization: true,
        supports_on_disk_vectors: true,
        supports_on_disk_hnsw: true,
        supports_sparse_vectors: true,
        sparse_bm25_control_enabled: true,
        supports_payload_indexes: true,
        supports_grpc: true,
        max_retries: 1,
        health_deadline_micros: 1,
    })
    .expect_err("legacy /points/search cannot satisfy this contract");
    assert_eq!(
        error.diagnostic_code(),
        QdrantBackendDiagnosticCode::InvalidCapabilities
    );
}

#[test]
fn qdrant_backend_bm25_hybrid_cannot_claim_tantivy_replacement_without_conformance() {
    let policy = QdrantLexicalPolicy::new(
        LexicalOwnership::QdrantSparseControlOnly,
        LexicalConformanceFlag::NotClaimed,
        true,
    )
    .expect("control-only policy");
    assert!(policy.is_control_only());
    let error = policy
        .claim_tantivy_replacement()
        .expect_err("conformance is mandatory");
    assert_eq!(
        error.diagnostic_code(),
        QdrantBackendDiagnosticCode::LexicalConformanceRequired
    );
}

#[test]
fn qdrant_backend_serde_round_trip_revalidates_schema_and_rejects_invalid_identity() {
    let encoded = serde_json::to_string(&schema()).expect("schema serializes");
    let decoded: QdrantCollectionSchema = serde_json::from_str(&encoded).expect("revalidates");
    assert_eq!(decoded, schema());

    let invalid_collection = "\"UPPERCASE\"";
    assert!(serde_json::from_str::<CollectionName>(invalid_collection).is_err());
}

#[test]
fn qdrant_backend_diagnostic_display_and_debug_are_code_only() {
    let error = QdrantSearchPolicy::reject_unconditional_local_pre_search(
        ForbiddenLocalPreSearch::UnconditionalLocalDensePreSearch,
    )
    .expect_err("forbidden");
    assert_eq!(
        error.to_string(),
        "qdrant-backend.unconditional_local_pre_search_forbidden"
    );
    assert_eq!(
        format!("{error:?}"),
        "QdrantBackendError(unconditional_local_pre_search_forbidden)"
    );
}

#[test]
fn qdrant_backend_grpc_path_requires_official_query_api_named_vectors_and_indexes() {
    let requirements = GrpcPathRequirements::new(
        QdrantTransport::OfficialClientGrpc,
        QdrantQuerySurface::QueryApi,
        true,
        true,
    )
    .expect("enterprise target path");
    assert_eq!(requirements.query_surface(), QdrantQuerySurface::QueryApi);
    assert!(GrpcPathRequirements::new(
        QdrantTransport::TransitionalRestLegacy,
        QdrantQuerySurface::LegacyPointsSearch,
        false,
        false,
    )
    .is_err());
}
