use crate::diskann3::{FilterPredicate, PublicationGeneration, TypedMetadataValue, VectorSpaceId};
use crate::diskann3_backend::{
    CandidateRepresentation, CandidateScore, DiskAnnBackendDiagnosticCode, ExactVector,
    ExactVectorFetchRequest, ExactVectorFetchResponse, FullQualityGuarantee, GenerationContext,
    PredicatePlan, RangeSearchRequest, RawDistanceRange, SearchBudget, SearchBudgetBinding,
    SearchBudgetFields, SearchCandidate, SearchContext, SearchPage, StableVectorId,
    TopKSearchRequest, VectorInput, VectorMetric, VectorSpaceSpec,
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

fn search_context(generation: &GenerationContext) -> SearchContext {
    let predicate = PredicatePlan::new(vec![
        FilterPredicate::source("source-a").expect("bounded source predicate")
    ])
    .expect("predicate plan");
    SearchContext::new(generation.clone(), predicate).expect("search context")
}

fn top_k_request(generation: &GenerationContext, limit: usize) -> TopKSearchRequest {
    TopKSearchRequest::new(search_context(generation), vector_input(generation), limit)
        .expect("validated Top-K request")
}

fn candidate(
    vector_id: u64,
    generation: PublicationGeneration,
    metric: VectorMetric,
) -> SearchCandidate {
    SearchCandidate::new(
        StableVectorId::new(vector_id).expect("stable ID"),
        generation,
        CandidateScore::new(metric, 0.25, 0.75).expect("candidate score"),
    )
}

fn exact_vector(context: &GenerationContext, vector_id: u64) -> ExactVector {
    ExactVector::new(
        context,
        StableVectorId::new(vector_id).expect("stable ID"),
        vector_input(context),
    )
    .expect("exact vector")
}

fn exact_request(generation: &GenerationContext, vector_ids: &[u64]) -> ExactVectorFetchRequest {
    let request = top_k_request(generation, vector_ids.len());
    let page = SearchPage::from_top_k_request(
        &request,
        vector_ids
            .iter()
            .map(|vector_id| candidate(*vector_id, generation.generation(), VectorMetric::L2))
            .collect(),
    )
    .expect("validated final search page");
    ExactVectorFetchRequest::from_search_page(&page).expect("exact request from final search page")
}

#[test]
fn diskann3_backend_requires_exact_rescore_eligibility() {
    assert_eq!(
        FullQualityGuarantee::new(CandidateRepresentation::ProductQuantized, true, false)
            .expect_err("candidate-only representations require exact final rescoring")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::FullQualityViolation
    );
}

#[test]
fn diskann3_backend_issues_exact_rescore_candidates_only_from_search_pages() {
    let generation = generation_context();
    let request = top_k_request(&generation, 1);
    let page = SearchPage::from_top_k_request(
        &request,
        vec![candidate(11, generation.generation(), VectorMetric::L2)],
    )
    .expect("validated final search page");

    let exact_request =
        ExactVectorFetchRequest::from_search_page(&page).expect("issued exact rescore request");
    assert_eq!(
        exact_request
            .candidates()
            .iter()
            .map(|candidate| candidate.vector_id().value())
            .collect::<Vec<_>>(),
        vec![11]
    );
}

#[test]
fn diskann3_backend_rejects_forged_exact_rescore_candidates_before_issuance() {
    let generation = generation_context();
    let request = top_k_request(&generation, 1);
    let forged_generation = PublicationGeneration::new(8).expect("different generation");

    assert_eq!(
        SearchPage::from_top_k_request(
            &request,
            vec![candidate(12, forged_generation, VectorMetric::L2)],
        )
        .expect_err("a forged candidate cannot reach exact-rescore issuance")
        .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::GenerationMismatch
    );
}

#[test]
fn diskann3_backend_rejects_duplicate_exact_rescore_candidate_ids() {
    let generation = generation_context();
    let request = top_k_request(&generation, 2);
    let page = SearchPage::from_top_k_request(
        &request,
        vec![
            candidate(11, generation.generation(), VectorMetric::L2),
            candidate(11, generation.generation(), VectorMetric::L2),
        ],
    )
    .expect("page can represent provider output before exact-fetch validation");

    assert_eq!(
        ExactVectorFetchRequest::from_search_page(&page)
            .expect_err("exact rescore request IDs must be unique")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::InvalidExactRescoreRequest
    );
}

#[test]
fn diskann3_backend_binds_exact_vector_response_to_requested_ids() {
    let generation = generation_context();
    let request = exact_request(&generation, &[11, 12]);

    assert_eq!(
        ExactVectorFetchResponse::new(&request, vec![exact_vector(&generation, 11)])
            .err()
            .expect("a response missing a requested vector must fail closed")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::InvalidExactRescoreRequest
    );
    assert_eq!(
        ExactVectorFetchResponse::new(
            &request,
            vec![exact_vector(&generation, 11), exact_vector(&generation, 13)],
        )
        .err()
        .expect("a response with an extra non-requested ID must fail closed")
        .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::InvalidExactRescoreRequest
    );
    assert_eq!(
        ExactVectorFetchResponse::new(
            &request,
            vec![exact_vector(&generation, 11), exact_vector(&generation, 11)],
        )
        .err()
        .expect("a response must not repeat one requested ID")
        .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::InvalidExactRescoreRequest
    );
}

#[test]
fn diskann3_backend_rejects_unbounded_exact_vector_responses() {
    let generation = generation_context();
    let request = exact_request(&generation, &[11, 12, 13, 14, 15]);
    let vectors = (11..=16)
        .map(|vector_id| exact_vector(&generation, vector_id))
        .collect();

    assert_eq!(
        ExactVectorFetchResponse::new(&request, vectors)
            .err()
            .expect("exact response cardinality must not exceed the rescore budget")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::InvalidExactRescoreRequest
    );
}

#[test]
fn diskann3_backend_accepts_exact_vector_response_matching_request() {
    let generation = generation_context();
    let request = exact_request(&generation, &[11, 12]);

    let response = ExactVectorFetchResponse::new(
        &request,
        vec![exact_vector(&generation, 11), exact_vector(&generation, 12)],
    )
    .expect("matching exact vector response");
    assert_eq!(response.vectors().len(), 2);
}

#[test]
fn diskann3_backend_search_pages_cannot_widen_request_limit() {
    let generation = generation_context();
    let request = top_k_request(&generation, 1);

    assert_eq!(
        SearchPage::from_top_k_request(
            &request,
            vec![
                candidate(11, generation.generation(), VectorMetric::L2),
                candidate(12, generation.generation(), VectorMetric::L2),
            ],
        )
        .expect_err("a one-result request cannot return the broader budget limit")
        .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::InvalidSearchRequest
    );
}

#[test]
fn diskann3_backend_rejects_metric_mismatched_candidate_scores() {
    let generation = generation_context();
    let request = top_k_request(&generation, 1);

    assert_eq!(
        SearchPage::from_top_k_request(
            &request,
            vec![candidate(11, generation.generation(), VectorMetric::Cosine)],
        )
        .expect_err("candidate score metrics must match the vector-space metric")
        .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::InvalidCandidateScore
    );
    assert_eq!(
        CandidateScore::new(VectorMetric::L2, -0.25, 0.75)
            .expect_err("negative L2 raw distances are invalid")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::InvalidCandidateScore
    );
}

#[test]
fn diskann3_backend_rejects_invalid_or_mismatched_distance_ranges() {
    assert_eq!(
        RawDistanceRange::new(VectorMetric::L2, -0.25, 1.0)
            .expect_err("negative L2 ranges must fail closed")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::InvalidDistanceRange
    );

    let generation = generation_context();
    assert_eq!(
        RangeSearchRequest::new(
            search_context(&generation),
            vector_input(&generation),
            RawDistanceRange::new(VectorMetric::Cosine, 0.0, 1.0).expect("valid cosine range"),
            1,
        )
        .err()
        .expect("range metric must match the vector-space metric")
        .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::InvalidDistanceRange
    );
}

#[test]
fn diskann3_backend_rejects_invalid_search_and_predicate_requests() {
    let generation = generation_context();
    assert_eq!(
        TopKSearchRequest::new(search_context(&generation), vector_input(&generation), 0)
            .err()
            .expect("zero result limit must fail closed")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::InvalidSearchRequest
    );

    let filters = (0..=PredicatePlan::MAX_FILTERS)
        .map(|index| {
            FilterPredicate::tenant(format!("tenant-{index}")).expect("bounded tenant predicate")
        })
        .collect();
    assert_eq!(
        PredicatePlan::new(filters)
            .expect_err("unbounded predicate plans must fail closed")
            .diagnostic_code(),
        DiskAnnBackendDiagnosticCode::InvalidPredicatePlan
    );
}

#[test]
fn diskann3_backend_redacts_predicate_and_search_context_debug() {
    let tenant_secret = "tenant-secret";
    let acl_secret = "group:classified";
    let metadata_secret = "metadata-secret";
    let predicate = PredicatePlan::new(vec![
        FilterPredicate::tenant(tenant_secret).expect("tenant predicate"),
        FilterPredicate::acl(acl_secret).expect("ACL predicate"),
        FilterPredicate::metadata_eq(
            "classification",
            TypedMetadataValue::String(metadata_secret.to_owned()),
        )
        .expect("metadata predicate"),
    ])
    .expect("predicate plan");
    let context =
        SearchContext::new(generation_context(), predicate.clone()).expect("search context");

    let rendered = format!("{predicate:?} {context:?}");
    for secret in [tenant_secret, acl_secret, metadata_secret] {
        assert!(
            !rendered.contains(secret),
            "redacted debug output must not contain {secret}"
        );
    }
    assert!(rendered.contains("REDACTED"));
}
