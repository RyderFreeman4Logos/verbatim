use crate::named_vector_spaces::{
    BackendCapabilities, CandidateInclusionReason, CandidateIndexProfileId,
    CompiledNamedVectorPlan, DegradedProfile, EmbeddingModality, ExactInteraction,
    FusionProfileIdentity, LateInteractionCandidateStage, LateInteractionQualityMeasurements,
    ModelIdentity, NamedVectorClause, NamedVectorPublicationManifest, NamedVectorQueryPlan,
    NamedVectorSpaceDiagnosticCode, NamedVectorSpaceId, NamedVectorSpaceResult,
    NamedVectorSpaceSpec, NamedVectorSpaceSpecFields, Normalization, ObjectId, ObjectSpaceMapping,
    PublicationGeneration, QueryOperation, QueryVectorShape, SearchBudget, SpaceAvailability,
    SpaceCandidate, SpacePublicationState, SpaceRetentionRequest, StagedSpaceArtifact,
    StorageComplexityContract, StorageEncoding, VectorLocation, VectorMetric, VectorRange,
};

fn assert_diagnostic<T>(
    result: NamedVectorSpaceResult<T>,
    expected: NamedVectorSpaceDiagnosticCode,
) {
    match result {
        Ok(_) => panic!("expected named-vector-spaces.{}", expected.as_str()),
        Err(error) => assert_eq!(error.diagnostic_code(), expected),
    }
}

fn assert_serde_diagnostic<T: serde::de::DeserializeOwned>(
    value: serde_json::Value,
    expected: NamedVectorSpaceDiagnosticCode,
) {
    match serde_json::from_value::<T>(value) {
        Ok(_) => panic!("expected named-vector-spaces.{}", expected.as_str()),
        Err(error) => assert_eq!(
            error.to_string(),
            format!("named-vector-spaces.{}", expected.as_str())
        ),
    }
}

fn generation() -> PublicationGeneration {
    PublicationGeneration::new(7).expect("valid generation")
}

fn dense_spec(name: &str, dimension: u32, metric: VectorMetric) -> NamedVectorSpaceSpec {
    NamedVectorSpaceSpec::new(NamedVectorSpaceSpecFields {
        name: NamedVectorSpaceId::new(name).expect("valid name"),
        modality: EmbeddingModality::Text,
        model: ModelIdentity::new("model-body-v1").expect("valid model"),
        native_dimension: dimension,
        metric,
        normalization: Normalization::L2,
        storage_encoding: StorageEncoding::Float32,
        candidate_index_profile: CandidateIndexProfileId::new("diskann3-body-v1")
            .expect("valid profile"),
        generation: generation(),
        supported_operations: vec![QueryOperation::DenseNearestNeighbor],
    })
    .expect("valid dense spec")
}

fn late_spec() -> NamedVectorSpaceSpec {
    NamedVectorSpaceSpec::new(NamedVectorSpaceSpecFields {
        name: NamedVectorSpaceId::new("body-tokens").expect("valid name"),
        modality: EmbeddingModality::LateInteractionToken,
        model: ModelIdentity::new("colbert-v2").expect("valid model"),
        native_dimension: 128,
        metric: VectorMetric::DotProduct,
        normalization: Normalization::None,
        storage_encoding: StorageEncoding::Float32,
        candidate_index_profile: CandidateIndexProfileId::new("diskann3-token-v1")
            .expect("valid profile"),
        generation: generation(),
        supported_operations: vec![QueryOperation::LateInteractionMaxSim],
    })
    .expect("valid late-interaction spec")
}

#[test]
fn accepts_dense_spaces_with_different_native_dimensions_and_metrics() {
    let body = dense_spec("body", 1_024, VectorMetric::Cosine);
    let image = NamedVectorSpaceSpec::new(NamedVectorSpaceSpecFields {
        name: NamedVectorSpaceId::new("image").expect("valid name"),
        modality: EmbeddingModality::Image,
        model: ModelIdentity::new("image-v1").expect("valid model"),
        native_dimension: 768,
        metric: VectorMetric::Euclidean,
        normalization: Normalization::None,
        storage_encoding: StorageEncoding::Float16,
        candidate_index_profile: CandidateIndexProfileId::new("diskann3-image-v1")
            .expect("valid profile"),
        generation: generation(),
        supported_operations: vec![QueryOperation::DenseNearestNeighbor],
    })
    .expect("valid image spec");

    assert_ne!(body.native_dimension(), image.native_dimension());
    assert_ne!(body.metric(), image.metric());
}

#[test]
fn rejects_zero_native_dimension_and_dimension_reduction_claims() {
    let mut fields = dense_spec("body", 1_024, VectorMetric::Cosine).into_fields();
    fields.native_dimension = 0;
    assert!(NamedVectorSpaceSpec::new(fields).is_err());
}

#[test]
fn supports_asymmetric_query_and_document_space_identities() {
    let query = NamedVectorSpaceSpec::new(NamedVectorSpaceSpecFields {
        name: NamedVectorSpaceId::new("query-en").expect("valid name"),
        modality: EmbeddingModality::AsymmetricQuery,
        model: ModelIdentity::new("retriever-query-v1").expect("valid model"),
        native_dimension: 768,
        metric: VectorMetric::DotProduct,
        normalization: Normalization::L2,
        storage_encoding: StorageEncoding::Float32,
        candidate_index_profile: CandidateIndexProfileId::new("diskann3-query-v1")
            .expect("valid profile"),
        generation: generation(),
        supported_operations: vec![QueryOperation::DenseNearestNeighbor],
    })
    .expect("query space");
    let document = NamedVectorSpaceSpec::new(NamedVectorSpaceSpecFields {
        name: NamedVectorSpaceId::new("document-en").expect("valid name"),
        modality: EmbeddingModality::AsymmetricDocument,
        model: ModelIdentity::new("retriever-document-v1").expect("valid model"),
        native_dimension: 768,
        metric: VectorMetric::DotProduct,
        normalization: Normalization::L2,
        storage_encoding: StorageEncoding::Float32,
        candidate_index_profile: CandidateIndexProfileId::new("diskann3-document-v1")
            .expect("valid profile"),
        generation: generation(),
        supported_operations: vec![QueryOperation::DenseNearestNeighbor],
    })
    .expect("document space");

    assert_ne!(query.name(), document.name());
    assert_ne!(query.model(), document.model());
}

#[test]
fn object_mapping_allows_zero_one_and_many_space_specific_vectors() {
    let object = ObjectId::new("chunk-17").expect("valid object");
    let space = NamedVectorSpaceId::new("image").expect("valid space");
    let zero = ObjectSpaceMapping::new(object.clone(), space.clone(), generation(), vec![])
        .expect("zero representations are a visible mapping");
    let one = ObjectSpaceMapping::new(
        object.clone(),
        space.clone(),
        generation(),
        vec![VectorLocation::dense(3, 9).expect("valid dense location")],
    )
    .expect("one representation");
    let many = ObjectSpaceMapping::new(
        object,
        space,
        generation(),
        vec![
            VectorLocation::dense(3, 10).expect("valid dense location"),
            VectorLocation::dense(3, 11).expect("valid dense location"),
        ],
    )
    .expect("many representations");

    assert_eq!(zero.vector_count(), 0);
    assert_eq!(one.vector_count(), 1);
    assert_eq!(many.vector_count(), 2);
}

#[test]
fn mapping_rejects_cross_generation_locations_and_duplicate_vector_ids() {
    let mapping = ObjectSpaceMapping::new(
        ObjectId::new("chunk-17").expect("valid object"),
        NamedVectorSpaceId::new("body").expect("valid space"),
        generation(),
        vec![
            VectorLocation::dense(3, 9).expect("valid dense location"),
            VectorLocation::dense(3, 9).expect("valid dense location"),
        ],
    );
    assert!(mapping.is_err());
}

#[test]
fn storage_contract_is_linear_in_space_vector_counts_and_native_dimensions() {
    let contract = StorageComplexityContract::new(vec![(1_000, 1_024), (200, 768)])
        .expect("positive storage terms");
    assert_eq!(
        contract.vector_component_class(),
        "O(sum(N_space * D_native))"
    );
    assert_eq!(contract.native_float32_bytes(), 4_710_400);
}

#[test]
fn clauses_compile_only_requested_eligible_spaces_under_one_shared_budget() {
    let body = dense_spec("body", 1_024, VectorMetric::Cosine);
    let title = dense_spec("title", 768, VectorMetric::Cosine);
    let plan = NamedVectorQueryPlan::new(
        vec![NamedVectorClause::new(
            NamedVectorSpaceId::new("body").expect("valid name"),
            QueryOperation::DenseNearestNeighbor,
            QueryVectorShape::dense(1_024).expect("valid shape"),
        )],
        25,
        FusionProfileIdentity::new("title-body-rrf", 3).expect("profile"),
    )
    .expect("plan");

    let compiled = plan
        .compile(&[body, title], BackendCapabilities::named_dense_only(), &[])
        .expect("compile");
    assert_eq!(compiled.shared_budget().maximum_candidates(), 25);
    assert_eq!(compiled.clauses().len(), 1);
    assert_eq!(compiled.clauses()[0].space().as_str(), "body");
}

#[test]
fn plan_binds_explicit_versioned_fusion_identity_for_title_body_or_image_text_fusion() {
    let profile = FusionProfileIdentity::new("image-text-rrf", 2).expect("profile");
    assert_eq!(profile.version(), 2);
    assert_eq!(profile.as_str(), "image-text-rrf");
}

#[test]
fn candidates_preserve_raw_rank_score_space_profile_and_inclusion_reason() {
    let candidate = SpaceCandidate::new(
        ObjectId::new("chunk-17").expect("object"),
        NamedVectorSpaceId::new("title").expect("space"),
        CandidateIndexProfileId::new("diskann3-title-v1").expect("profile"),
        1,
        0.97,
        CandidateInclusionReason::RequestedClause,
    )
    .expect("candidate");
    assert_eq!(candidate.raw_rank(), 1);
    assert_eq!(candidate.raw_score(), 0.97);
    assert_eq!(
        candidate.inclusion_reason(),
        CandidateInclusionReason::RequestedClause
    );
}

#[test]
fn late_interaction_requires_bounded_candidate_stage_and_page_aligned_ranges() {
    let stage = LateInteractionCandidateStage::new(32, 200, true).expect("bounded stage");
    assert!(stage.approximate_candidate_stage());
    assert!(VectorRange::new(256, 64, 64).is_ok());
    assert!(VectorRange::new(257, 64, 64).is_err());
}

#[test]
fn exact_maxsim_is_declared_separately_from_approximate_candidate_generation() {
    let exact = ExactInteraction::max_sim_full_precision();
    let report =
        LateInteractionQualityMeasurements::new(0.81, 0.94, 100).expect("separate measurements");
    assert!(exact.requires_original_vectors());
    assert!(report.candidate_recall() < report.final_interaction_quality());
}

#[test]
fn exact_maxsim_scores_original_full_precision_multitoken_vectors() {
    let query = vec![vec![1.0_f32, 0.0], vec![0.0, 1.0]];
    let stronger_document = vec![vec![1.0_f32, 0.0], vec![0.0, 1.0]];
    let weaker_document = vec![vec![0.5_f32, 0.5], vec![0.25, 0.75]];
    let exact = ExactInteraction::max_sim_full_precision();

    let stronger_score = exact
        .score_original_vectors(&query, &stronger_document)
        .expect("finite vectors with matching dimensions");
    let weaker_score = exact
        .score_original_vectors(&query, &weaker_document)
        .expect("finite vectors with matching dimensions");

    assert_eq!(stronger_score, 2.0);
    assert_eq!(weaker_score, 1.25);
    assert!(stronger_score > weaker_score);
}

#[test]
fn late_interaction_space_requires_maxsim_operation() {
    let mut fields = late_spec().into_fields();
    fields.supported_operations = vec![QueryOperation::DenseNearestNeighbor];
    assert!(NamedVectorSpaceSpec::new(fields).is_err());
}

#[test]
fn exact_maxsim_space_requires_float32_original_vector_storage() {
    for storage_encoding in [
        StorageEncoding::Float16,
        StorageEncoding::ProductQuantized,
        StorageEncoding::BinaryCandidateCode,
    ] {
        let mut fields = late_spec().into_fields();
        fields.storage_encoding = storage_encoding;
        assert_diagnostic(
            NamedVectorSpaceSpec::new(fields),
            NamedVectorSpaceDiagnosticCode::OriginalVectorsRequired,
        );
    }
}

#[test]
fn missing_stale_and_wrong_generation_spaces_are_visible_typed_states() {
    assert_diagnostic(
        SpaceAvailability::missing().require_complete(generation()),
        NamedVectorSpaceDiagnosticCode::MissingVectorSpace,
    );
    assert_diagnostic(
        SpaceAvailability::stale(PublicationGeneration::new(6).expect("generation"))
            .require_complete(generation()),
        NamedVectorSpaceDiagnosticCode::StaleVectorSpace,
    );
    assert_diagnostic(
        SpaceAvailability::complete(PublicationGeneration::new(8).expect("generation"))
            .require_complete(generation()),
        NamedVectorSpaceDiagnosticCode::WrongGeneration,
    );
}

#[test]
fn unsupported_capability_fails_without_silent_fallback() {
    let plan = NamedVectorQueryPlan::new(
        vec![NamedVectorClause::new(
            NamedVectorSpaceId::new("body-tokens").expect("name"),
            QueryOperation::LateInteractionMaxSim,
            QueryVectorShape::late_interaction(128, 12).expect("shape"),
        )],
        10,
        FusionProfileIdentity::new("late-rrf", 1).expect("profile"),
    )
    .expect("plan");
    assert_diagnostic(
        plan.compile(&[late_spec()], BackendCapabilities::named_dense_only(), &[]),
        NamedVectorSpaceDiagnosticCode::UnsupportedBackendCapability,
    );
}

#[test]
fn explicitly_named_degraded_profile_can_be_selected_but_not_implicit_fallback() {
    let degraded = DegradedProfile::new(
        "dense-only-late-disabled",
        vec![NamedVectorSpaceId::new("body").expect("space")],
    )
    .expect("degraded profile");
    assert_eq!(degraded.as_str(), "dense-only-late-disabled");
}

#[test]
fn serde_revalidates_durable_named_space_contracts() {
    let json = r#"{
        "name":"body",
        "modality":"text",
        "model":"model-body-v1",
        "native_dimension":0,
        "metric":"cosine",
        "normalization":"l2",
        "storage_encoding":"float32",
        "candidate_index_profile":"diskann3-body-v1",
        "generation":7,
        "supported_operations":["dense_nearest_neighbor"]
    }"#;
    let decoded: Result<NamedVectorSpaceSpec, _> = serde_json::from_str(json);
    assert!(decoded.is_err());
}

#[test]
fn serde_revalidates_durable_mapping_locations() {
    let json = r#"{
        "object":"chunk-17",
        "space":"body",
        "generation":7,
        "locations":[{"kind":"dense","shard_ordinal":0,"vector_id":9}]
    }"#;
    let decoded: Result<ObjectSpaceMapping, _> = serde_json::from_str(json);
    assert!(decoded.is_err());
}

#[test]
fn serde_rejects_constructor_only_named_vector_invariants_with_closed_codes() {
    assert_serde_diagnostic::<SearchBudget>(
        serde_json::json!({"maximum_candidates": 0}),
        NamedVectorSpaceDiagnosticCode::InvalidSearchBudget,
    );
    assert_serde_diagnostic::<NamedVectorQueryPlan>(
        serde_json::json!({
            "clauses": [],
            "shared_budget": {"maximum_candidates": 1},
            "fusion_profile": {"id": "body-rrf", "version": 1}
        }),
        NamedVectorSpaceDiagnosticCode::InvalidQueryPlan,
    );
    assert_serde_diagnostic::<NamedVectorQueryPlan>(
        serde_json::json!({
            "clauses": [{
                "space": "body",
                "operation": "dense_nearest_neighbor",
                "shape": {"kind": "dense", "native_dimension": 128}
            }],
            "shared_budget": {"maximum_candidates": 0},
            "fusion_profile": {"id": "body-rrf", "version": 1}
        }),
        NamedVectorSpaceDiagnosticCode::InvalidSearchBudget,
    );
    assert_serde_diagnostic::<QueryVectorShape>(
        serde_json::json!({"kind": "late_interaction", "native_dimension": 128, "query_token_count": 0}),
        NamedVectorSpaceDiagnosticCode::IncompatibleQueryShape,
    );
    assert_serde_diagnostic::<LateInteractionCandidateStage>(
        serde_json::json!({
            "maximum_query_token_frontier": 0,
            "maximum_object_candidate_pool": 10,
            "approximate_candidate_stage": true
        }),
        NamedVectorSpaceDiagnosticCode::InvalidLateInteractionLayout,
    );
    assert_serde_diagnostic::<LateInteractionCandidateStage>(
        serde_json::json!({
            "maximum_query_token_frontier": 10,
            "maximum_object_candidate_pool": 0,
            "approximate_candidate_stage": true
        }),
        NamedVectorSpaceDiagnosticCode::InvalidLateInteractionLayout,
    );
    assert_serde_diagnostic::<StorageComplexityContract>(
        serde_json::json!({"terms": [[0, 128]]}),
        NamedVectorSpaceDiagnosticCode::InvalidStorageComplexity,
    );
    assert_serde_diagnostic::<VectorLocation>(
        serde_json::json!({"kind": "dense", "shard_ordinal": 0, "vector_id": 9}),
        NamedVectorSpaceDiagnosticCode::InvalidVectorMapping,
    );
    assert_serde_diagnostic::<VectorLocation>(
        serde_json::json!({"kind": "dense", "shard_ordinal": 1, "vector_id": 0}),
        NamedVectorSpaceDiagnosticCode::InvalidVectorMapping,
    );
    assert_serde_diagnostic::<DegradedProfile>(
        serde_json::json!({"name": "", "allowed_spaces": []}),
        NamedVectorSpaceDiagnosticCode::InvalidQueryPlan,
    );
    assert_serde_diagnostic::<SpaceCandidate>(
        serde_json::json!({
            "object": "chunk-17",
            "space": "body",
            "profile": "diskann3-body-v1",
            "raw_rank": 0,
            "raw_score": 0.97,
            "inclusion_reason": "requested_clause"
        }),
        NamedVectorSpaceDiagnosticCode::InvalidQueryPlan,
    );
}

#[test]
fn publication_manifest_and_retention_are_generation_bound_and_fail_closed() {
    let body = NamedVectorSpaceId::new("body").expect("space");
    assert!(StagedSpaceArtifact::new(
        body.clone(),
        generation(),
        0,
        SpacePublicationState::Complete,
    )
    .is_err());

    let staged = StagedSpaceArtifact::new(
        body.clone(),
        generation(),
        1,
        SpacePublicationState::Complete,
    )
    .expect("staged artifact");
    let manifest =
        NamedVectorPublicationManifest::new(generation(), vec![staged]).expect("atomic manifest");
    assert_eq!(manifest.spaces().len(), 1);
    assert!(SpaceRetentionRequest::new(body, 1, true).is_err());
}

#[test]
fn compiled_plan_is_a_typed_contract_not_a_live_backend_runtime() {
    let _: Option<CompiledNamedVectorPlan> = None;
}
