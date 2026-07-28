use verbatim_core::diskann3::{FilterPredicate, PublicationGeneration, VectorSpaceId};
use verbatim_core::diskann3_backend::{
    CandidateScore, ExactVectorFetchRequest, GenerationContext, PredicatePlan, SearchBudget,
    SearchBudgetBinding, SearchBudgetFields, SearchCandidate, SearchContext, SearchPage,
    StableVectorId, TopKSearchRequest, VectorInput, VectorMetric, VectorSpaceSpec,
};
use verbatim_core::types::EmbeddingProfileId;

fn main() {
    let budget = SearchBudget::new(SearchBudgetFields {
        result_limit: 1,
        dense_candidate_limit: 1,
        lexical_candidate_limit: 1,
        exact_candidate_limit: 1,
        graph_candidate_limit: 1,
        fused_pool_limit: 1,
        rerank_candidate_limit: 1,
        full_precision_rescore_limit: 1,
        hydration_limit: 1,
        max_ssd_pages: 1,
        max_bytes_read: 1,
        max_cpu_micros: 1,
        max_work_units: 1,
        max_wall_time_micros: 1,
        max_concurrent_stages: 1,
        max_stage_attempts: 1,
        debug_record_limit: 1,
    })
    .expect("valid search budget");
    let generation = PublicationGeneration::new(7).expect("valid generation");
    let context = GenerationContext::new(
        VectorSpaceSpec::new(
            VectorSpaceId::new("text-default").expect("valid vector space"),
            EmbeddingProfileId::new("default").expect("valid profile"),
            VectorMetric::L2,
        )
        .expect("valid vector space spec"),
        generation,
        SearchBudgetBinding::new(budget, budget).expect("bounded budget"),
    )
    .expect("valid generation context");
    let search_context = SearchContext::new(
        context.clone(),
        PredicatePlan::new(vec![FilterPredicate::source("source-a").expect("predicate")])
            .expect("predicate plan"),
    )
    .expect("search context");
    let request = TopKSearchRequest::new(
        search_context,
        VectorInput::new(
            vec![0.25_f32; VectorSpaceSpec::DIMENSION],
            context.vector_space().profile_id().clone(),
            context.generation(),
        ),
        1,
    )
    .expect("top-k request");

    let candidate = SearchCandidate::new(
        StableVectorId::new(999_999).expect("arbitrary same-generation ID"),
        context.generation(),
        CandidateScore::new(VectorMetric::L2, 0.25, 0.75).expect("candidate score"),
    );
    let page = SearchPage::from_top_k_request(&request, vec![candidate])
        .expect("same-generation caller-built page");
    let _exact =
        ExactVectorFetchRequest::from_search_page(&page).expect("forged exact-vector request");
}
