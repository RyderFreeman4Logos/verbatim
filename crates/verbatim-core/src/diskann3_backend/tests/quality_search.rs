use crate::diskann3::{FilterPredicate, PublicationGeneration, VectorSpaceId};
use crate::diskann3_backend::{
    CandidateRepresentation, DiskAnnBackendDiagnosticCode, ExactRescoreCandidate,
    ExactVectorFetchRequest, FullQualityGuarantee, GenerationContext, PredicatePlan,
    RangeSearchRequest, RawDistanceRange, SearchBudget, SearchBudgetBinding, SearchBudgetFields,
    SearchContext, TopKSearchRequest, VectorInput, VectorMetric, VectorSpaceSpec,
};
use crate::types::EmbeddingProfileId;

fn vector_space() -> VectorSpaceSpec {
    VectorSpaceSpec::new(
        VectorSpaceId::new("text-default").expect("vector space"),
        EmbeddingProfileId::new("default").expect("embedding profile"),
        VectorMetric::L2,
    )
    .expect("vector space")
}

fn search_budget() -> SearchBudget {
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
    .expect("search budget")
}

fn generation_context() -> GenerationContext {
    let budget = search_budget();
    GenerationContext::new(
        vector_space(),
        PublicationGeneration::new(7).expect("generation"),
        SearchBudgetBinding::new(budget, budget).expect("narrow budget binding"),
    )
    .expect("generation context")
}

fn vector_input(context: &GenerationContext) -> VectorInput {
    VectorInput::new(
        vec![0.25_f32; VectorSpaceSpec::DIMENSION],
        context.vector_space().profile_id().clone(),
        context.generation(),
    )
}

#[test]
fn diskann3_backend_requires_exact_rescore_eligibility() {
    assert_eq!(
        FullQualityGuarantee::new(CandidateRepresentation::ProductQuantized, true, false)
            .expect_err("candidate-only representations require exact final rescoring")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::FullQualityViolation
    );

    let context = generation_context();
    let ineligible = ExactRescoreCandidate::new(
        crate::diskann3_backend::StableVectorId::new(11).expect("stable ID"),
        context.generation(),
        false,
    );
    assert_eq!(
        ExactVectorFetchRequest::new(context, vec![ineligible])
            .expect_err("final candidates without original-vector access must be rejected")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::ExactRescoreIneligible
    );
}

#[test]
fn diskann3_backend_accepts_validated_predicate_search_requests() {
    let generation = generation_context();
    let predicate = PredicatePlan::new(vec![
        FilterPredicate::source("source-a").expect("bounded source predicate")
    ])
    .expect("predicate plan");
    let context = SearchContext::new(generation.clone(), predicate).expect("search context");

    TopKSearchRequest::new(context.clone(), vector_input(&generation), 5)
        .expect("validated predicate-aware Top-K request");
    RangeSearchRequest::new(
        context,
        vector_input(&generation),
        RawDistanceRange::new(0.0, 1.0).expect("finite ordered range"),
        5,
    )
    .expect("validated predicate-aware range request");
}
