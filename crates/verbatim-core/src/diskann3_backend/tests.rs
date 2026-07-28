#[path = "tests/lifecycle.rs"]
mod lifecycle;
#[path = "tests/quality_search.rs"]
mod quality_search;

use crate::diskann3::{PublicationGeneration, VectorSpaceId};
use crate::diskann3_backend::{
    ChunkIdMapping, ChunkIdMappingEntry, DiskAnnBackendDiagnosticCode, MappingVersion,
    SearchBudget, SearchBudgetBinding, SearchBudgetFields, StableVectorId, VectorInput,
    VectorMetric, VectorNormalization, VectorSpaceSpec,
};
use crate::types::{ChunkId, EmbeddingProfileId};

fn vector_space(metric: VectorMetric) -> VectorSpaceSpec {
    VectorSpaceSpec::new(
        VectorSpaceId::new("text-default").expect("vector space"),
        EmbeddingProfileId::new("default").expect("embedding profile"),
        metric,
    )
    .expect("validated vector space")
}

fn search_budget(max_ssd_pages: u64) -> SearchBudget {
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
        max_ssd_pages,
        max_bytes_read: 1_024,
        max_cpu_micros: 1_024,
        max_work_units: 1_024,
        max_wall_time_micros: 1_024,
        max_concurrent_stages: 1,
        max_stage_attempts: 1,
        debug_record_limit: 5,
    })
    .expect("valid hard-bounded search budget")
}

#[test]
fn diskann3_backend_rejects_wrong_dimension_and_non_finite_vectors() {
    let space = vector_space(VectorMetric::L2);

    let wrong_dimension = vec![0.25_f32; VectorSpaceSpec::DIMENSION - 1];
    assert_eq!(
        space
            .validate_vector(&wrong_dimension)
            .expect_err("short vectors must fail closed")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::VectorDimensionMismatch
    );

    let non_finite = vec![f32::NAN; VectorSpaceSpec::DIMENSION];
    assert_eq!(
        space
            .validate_vector(&non_finite)
            .expect_err("non-finite vectors must fail closed")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::NonFiniteVector
    );
}

#[test]
fn diskann3_backend_enforces_metric_normalization_and_zero_rules() {
    let zero = vec![0.0_f32; VectorSpaceSpec::DIMENSION];
    for metric in [VectorMetric::Cosine, VectorMetric::Dot, VectorMetric::L2] {
        assert_eq!(
            vector_space(metric)
                .validate_vector(&zero)
                .expect_err("zero vectors cannot enter any metric space")
                .diagnostic_code(),
            DiskAnnBackendDiagnosticCode::ZeroVector
        );
    }

    let mut non_unit = vec![0.0_f32; VectorSpaceSpec::DIMENSION];
    non_unit[0] = 2.0;
    assert_eq!(
        vector_space(VectorMetric::Cosine)
            .validate_vector(&non_unit)
            .expect_err("cosine vectors must carry the declared unit normalization")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::MetricNormalizationMismatch
    );
    vector_space(VectorMetric::Dot)
        .validate_vector(&non_unit)
        .expect("dot product preserves magnitude");
    vector_space(VectorMetric::L2)
        .validate_vector(&non_unit)
        .expect("L2 preserves magnitude");

    assert_eq!(
        VectorMetric::Cosine.normalization(),
        VectorNormalization::UnitL2
    );
    assert_eq!(
        VectorMetric::Dot.normalization(),
        VectorNormalization::PreserveMagnitude
    );
}

#[test]
fn diskann3_backend_rejects_wrong_profile_and_generation_bindings() {
    let space = vector_space(VectorMetric::L2);
    let expected_generation = PublicationGeneration::new(7).expect("nonzero generation");
    let valid_values = vec![0.25_f32; VectorSpaceSpec::DIMENSION];

    let wrong_profile = VectorInput::new(
        valid_values.clone(),
        EmbeddingProfileId::new("other-profile").expect("embedding profile"),
        expected_generation,
    );
    assert_eq!(
        space
            .validate_input(&wrong_profile, expected_generation)
            .expect_err("vectors from another profile must be rejected")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::ProfileMismatch
    );

    let wrong_generation = VectorInput::new(
        valid_values,
        EmbeddingProfileId::new("default").expect("embedding profile"),
        PublicationGeneration::new(8).expect("nonzero generation"),
    );
    assert_eq!(
        space
            .validate_input(&wrong_generation, expected_generation)
            .expect_err("vectors from another publication generation must be rejected")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::GenerationMismatch
    );
}

#[test]
fn diskann3_backend_versions_chunk_id_mappings() {
    let space = vector_space(VectorMetric::L2);
    let generation = PublicationGeneration::new(7).expect("nonzero generation");
    let vector_id = StableVectorId::new(42).expect("positive stable vector ID");
    let mapping = ChunkIdMapping::new(
        MappingVersion::new(3).expect("positive mapping version"),
        space.vector_space_id().clone(),
        space.profile_id().clone(),
        generation,
        vec![ChunkIdMappingEntry::new(
            vector_id,
            ChunkId("chunk-42".to_owned()),
        )],
    )
    .expect("versioned chunk mapping");

    assert_eq!(mapping.version().value(), 3);
    assert_eq!(mapping.generation(), generation);
    assert_eq!(
        mapping
            .chunk_id(vector_id)
            .expect("stable vector ID must resolve"),
        &ChunkId("chunk-42".to_owned())
    );
}

#[test]
fn diskann3_backend_rejects_widened_operation_budget() {
    let caller_budget = search_budget(10);
    let widened_operation_budget = search_budget(11);

    assert_eq!(
        SearchBudgetBinding::new(caller_budget, widened_operation_budget)
            .expect_err("an adapter operation may narrow but never widen caller budget")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::SearchBudgetWidened
    );
}
