use std::cell::RefCell;

use crate::overfetch::{
    decode_retrieval_plan_json, decode_search_budget_json, encode_retrieval_plan_json,
    encode_search_budget_json, AdaptiveOverfetchPolicy, AdaptiveOverfetchPolicyFields,
    BatchHydrationPort, BoundedRetrievalContract, CandidateId, CandidateValidation,
    ComplexityInvariant, CountPort, DebugOutput, DiagnosticMode, FullHydration, FusedCandidates,
    HydrationBatch, HydrationBatchKind, LifecycleState, OverfetchError, OverfetchResult,
    PrimaryBackendOutcome, PrimaryBackendSelection, RetrievalBackend, RetrievalCandidate,
    RetrievalFilters, RetrievalPlan, RetrieverCandidates, SearchBudget, SearchBudgetFields,
    StatementCountInstrumentation, StrictFilter, StrictFilterSupport, TypedBackendFailure,
    ValidatedCandidates,
};

fn budget() -> SearchBudget {
    SearchBudget::new(SearchBudgetFields {
        dense_candidate_k: 4,
        lexical_candidate_k: 3,
        exact_candidate_k: 2,
        graph_candidate_k: 2,
        fused_pool_size: 5,
        rerank_input_size: 4,
        final_hydration_list_size: 2,
        debug_output_size: 3,
    })
    .expect("valid fixed budget")
}

fn candidate(id: &str, score: f32) -> RetrievalCandidate {
    RetrievalCandidate::new(CandidateId::new(id).expect("valid candidate id"), score)
        .expect("finite candidate score")
}

#[test]
fn overfetch_contract_budget_and_plan_round_trip_revalidate_all_dimensions() {
    let budget = budget();
    let filters = RetrievalFilters::new(vec![
        StrictFilter::source("source-a").expect("source filter"),
        StrictFilter::collection("collection-a").expect("collection filter"),
        StrictFilter::tenant("tenant-a").expect("tenant filter"),
        StrictFilter::acl("group-a").expect("ACL filter"),
        StrictFilter::lifecycle(LifecycleState::Active),
    ])
    .expect("all strict predicates are bounded");
    let plan = RetrievalPlan::new(budget, filters)
        .expect("valid plan")
        .with_diagnostic_mode(DiagnosticMode::Full);

    let budget_json = encode_search_budget_json(&budget).expect("budget encodes");
    assert_eq!(
        decode_search_budget_json(&budget_json).expect("budget decodes"),
        budget
    );
    let plan_json = encode_retrieval_plan_json(&plan).expect("plan encodes");
    assert_eq!(
        decode_retrieval_plan_json(&plan_json).expect("plan decodes"),
        plan
    );

    let invalid_budget = r#"{
        "dense_candidate_k": 4,
        "lexical_candidate_k": 3,
        "exact_candidate_k": 2,
        "graph_candidate_k": 2,
        "fused_pool_size": 3,
        "rerank_input_size": 4,
        "final_hydration_list_size": 2,
        "debug_output_size": 3
    }"#;
    assert_eq!(
        decode_search_budget_json(invalid_budget).expect_err("rerank exceeds fusion"),
        OverfetchError::BudgetExceeded
    );
    assert_eq!(
        SearchBudget::new(SearchBudgetFields {
            dense_candidate_k: u32::MAX,
            lexical_candidate_k: 1,
            exact_candidate_k: 1,
            graph_candidate_k: 1,
            fused_pool_size: u32::MAX,
            rerank_input_size: u32::MAX,
            final_hydration_list_size: u32::MAX,
            debug_output_size: 1,
        })
        .expect_err("combined retriever budget cannot overflow"),
        OverfetchError::BudgetExceeded
    );

    assert_eq!(
        ComplexityInvariant::RETRIEVER_CANDIDATES,
        "O(k_dense + k_lexical + k_exact + k_graph)"
    );
    assert_eq!(
        ComplexityInvariant::FUSION,
        "O(candidate_budget log candidate_budget)"
    );
    assert_eq!(ComplexityInvariant::HYDRATION_SQL_CALLS, "O(1) batches");
    assert_eq!(
        ComplexityInvariant::HYDRATED_TEXT,
        "O(final_limit + bounded_rerank_candidates)"
    );
}

#[test]
fn overfetch_contract_hard_truncates_before_each_next_stage_and_debug_collection() {
    let budget = budget();
    let candidates = |prefix: &str| {
        (0..10)
            .map(|index| candidate(&format!("{prefix}-{index}"), index as f32))
            .collect::<Vec<_>>()
    };
    let retrieved = RetrieverCandidates::new(
        &budget,
        candidates("dense"),
        candidates("lexical"),
        candidates("exact"),
        candidates("graph"),
    )
    .expect("retriever candidates are bounded");
    assert_eq!(retrieved.dense().len(), 4);
    assert_eq!(retrieved.lexical().len(), 3);
    assert_eq!(retrieved.exact().len(), 2);
    assert_eq!(retrieved.graph().len(), 2);

    let fused = FusedCandidates::fuse_truncate(retrieved, &budget).expect("fused cap");
    assert_eq!(fused.candidates().len(), 5);

    let validations = fused
        .candidates()
        .iter()
        .cloned()
        .map(CandidateValidation::new)
        .collect::<OverfetchResult<Vec<_>>>()
        .expect("lightweight validation");
    let validated = ValidatedCandidates::new(validations, &budget).expect("rerank cap");
    assert_eq!(validated.candidates().len(), 4);
    assert_eq!(validated.for_hydration(&budget).len(), 2);

    let hydrated_too_many = validated
        .candidates()
        .iter()
        .cloned()
        .map(|validation| FullHydration::new(validation, ()))
        .collect::<Vec<_>>();
    assert_eq!(
        HydrationBatch::new(hydrated_too_many, &budget).expect_err("unbounded hydration"),
        OverfetchError::UnboundedHydration
    );

    let compact = DebugOutput::collect(DiagnosticMode::Compact, &budget, || 0..10)
        .expect("compact debug is capped");
    assert_eq!(compact.entries().len(), 3);

    let full = DebugOutput::collect(DiagnosticMode::Full, &budget, || 0..10)
        .expect("explicit full debug is still capped");
    assert_eq!(full.mode(), DiagnosticMode::Full);
    assert_eq!(full.entries().len(), 3);

    let debug_factory_called = std::cell::Cell::new(false);
    let disabled = DebugOutput::<u32>::collect(DiagnosticMode::Disabled, &budget, || {
        debug_factory_called.set(true);
        0..10
    })
    .expect("disabled debug remains empty");
    assert!(disabled.entries().is_empty());
    assert!(!debug_factory_called.get());
}

#[test]
fn overfetch_contract_adaptive_overfetch_and_unsupported_filters_fail_closed() {
    let policy = AdaptiveOverfetchPolicy::new(AdaptiveOverfetchPolicyFields {
        initial_candidate_k: 3,
        max_candidate_k: 8,
        growth_factor: 2,
        max_attempts: 3,
    })
    .expect("valid adaptive policy");

    assert_eq!(
        policy
            .candidate_k_for_attempt(0, 4, 1_000_000)
            .expect("initial bounded request"),
        3
    );
    assert_eq!(
        policy
            .candidate_k_for_attempt(1, 4, 1_000_000)
            .expect("budget remains the ceiling"),
        4
    );
    assert_eq!(
        StrictFilterSupport::Adaptive(policy)
            .candidate_k_for_attempt(4, 4, 1_000_000, 0)
            .expect("adaptive policy may request less than the hard cap"),
        3
    );
    assert_eq!(
        policy
            .candidate_k_for_attempt(0, 4, 3)
            .expect_err("corpus-sized top-k is forbidden"),
        OverfetchError::CorpusSizeTopKForbidden
    );

    assert_eq!(
        StrictFilterSupport::Unsupported
            .candidate_k_for_attempt(2, 4, 1_000_000, 0)
            .expect_err("unsupported strict predicate fails closed"),
        OverfetchError::UnsupportedStrictFilter
    );
}

#[test]
fn overfetch_contract_primary_backend_runs_first_and_fallback_is_conditional() {
    let selection =
        PrimaryBackendSelection::new(RetrievalBackend::Qdrant, Some(RetrievalBackend::LocalDense))
            .expect("distinct primary and fallback");

    assert_eq!(
        selection
            .validate_first_attempt(RetrievalBackend::LocalDense)
            .expect_err("fallback cannot pre-search before primary"),
        OverfetchError::PrimaryBackendRequired
    );
    selection
        .validate_first_attempt(RetrievalBackend::Qdrant)
        .expect("selected primary runs first");
    assert_eq!(
        selection
            .fallback_after(PrimaryBackendOutcome::Satisfied)
            .expect("success does not use fallback"),
        None
    );
    assert_eq!(
        selection
            .fallback_after(PrimaryBackendOutcome::DeclaredInsufficientResults)
            .expect("declared insufficiency permits fallback"),
        Some(RetrievalBackend::LocalDense)
    );
    assert_eq!(
        selection
            .fallback_after(PrimaryBackendOutcome::TypedFailure(
                TypedBackendFailure::Unavailable,
            ))
            .expect("typed failure permits fallback"),
        Some(RetrievalBackend::LocalDense)
    );

    let no_fallback = PrimaryBackendSelection::new(RetrievalBackend::Qdrant, None)
        .expect("primary-only selection");
    assert_eq!(
        no_fallback
            .fallback_after(PrimaryBackendOutcome::DeclaredInsufficientResults)
            .expect_err("no undeclared fallback is allowed"),
        OverfetchError::PrimaryBackendRequired
    );
}

#[test]
fn overfetch_contract_sql_statement_count_is_constant_and_detects_n_plus_one() {
    for corpus_size in [10_u64, 10_000, 1_000_000] {
        let counter = IndexedCountPort { corpus_size };
        assert_eq!(
            counter
                .count_indexed(&RetrievalFilters::default())
                .expect("indexed count"),
            corpus_size
        );

        let mut statements = StatementCountInstrumentation::new(5).expect("statement cap");
        for batch in HydrationBatchKind::ALL {
            statements
                .record_hydration_batch(batch)
                .expect("one statement per batch kind");
        }
        assert_eq!(statements.observed_statements(), 5);
    }

    let mut statements = StatementCountInstrumentation::new(6).expect("statement cap");
    statements
        .record_hydration_batch(HydrationBatchKind::ChunkHeaders)
        .expect("first header batch");
    assert_eq!(
        statements
            .record_hydration_batch(HydrationBatchKind::ChunkHeaders)
            .expect_err("a repeated batch is deterministic N+1"),
        OverfetchError::NPlusOneDetected
    );
}

#[test]
fn overfetch_contract_runs_the_required_pipeline_in_order() {
    let contract = ContractStub::default();
    let mut statements = StatementCountInstrumentation::new(5).expect("statement cap");
    let hydrated_count = contract
        .retrieve(&budget(), &RetrievalFilters::default(), &mut statements)
        .expect("bounded orchestration succeeds");

    assert_eq!(hydrated_count, 1);
    assert_eq!(
        contract.calls.borrow().as_slice(),
        [
            "plan",
            "execute_retrievers",
            "fuse_truncate",
            "validate_candidates",
            "hydrate_batch",
            "report",
        ]
    );
    assert_eq!(statements.observed_statements(), 5);
}

#[test]
fn overfetch_contract_errors_are_diagnostic_codes_without_input_leaks() {
    for error in OverfetchError::ALL {
        let debug = format!("{error:?}");
        let display = error.to_string();
        assert!(debug.len() < 64);
        assert!(display.starts_with("overfetch."));
        assert!(display.contains(error.diagnostic_code()));
    }

    let secret = "do-not-leak-this-input";
    let error = decode_search_budget_json(secret).expect_err("invalid JSON fails closed");
    assert_eq!(error, OverfetchError::BudgetExceeded);
    assert!(!format!("{error:?}").contains(secret));
    assert!(!error.to_string().contains(secret));
}

struct IndexedCountPort {
    corpus_size: u64,
}

impl CountPort for IndexedCountPort {
    fn count_indexed(&self, _filters: &RetrievalFilters) -> OverfetchResult<u64> {
        Ok(self.corpus_size)
    }
}

#[derive(Default)]
struct ContractStub {
    calls: RefCell<Vec<&'static str>>,
}

impl ContractStub {
    fn record(&self, call: &'static str) {
        self.calls.borrow_mut().push(call);
    }
}

impl CountPort for ContractStub {
    fn count_indexed(&self, _filters: &RetrievalFilters) -> OverfetchResult<u64> {
        Ok(1)
    }
}

impl BatchHydrationPort for ContractStub {
    type ChunkHeader = ();
    type ChunkBody = ();
    type ParentLink = ();
    type ChunkEvidenceLink = ();
    type EvidenceUnit = ();
    type Hydrated = ();

    fn fetch_chunk_headers(
        &self,
        candidates: &[CandidateValidation],
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Vec<Self::ChunkHeader>> {
        statements.record_hydration_batch(HydrationBatchKind::ChunkHeaders)?;
        Ok(vec![(); candidates.len()])
    }

    fn fetch_chunk_bodies(
        &self,
        candidates: &[CandidateValidation],
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Vec<Self::ChunkBody>> {
        statements.record_hydration_batch(HydrationBatchKind::ChunkBodies)?;
        Ok(vec![(); candidates.len()])
    }

    fn fetch_parent_links(
        &self,
        candidates: &[CandidateValidation],
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Vec<Self::ParentLink>> {
        statements.record_hydration_batch(HydrationBatchKind::ParentLinks)?;
        Ok(vec![(); candidates.len()])
    }

    fn fetch_chunk_evidence_links(
        &self,
        candidates: &[CandidateValidation],
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Vec<Self::ChunkEvidenceLink>> {
        statements.record_hydration_batch(HydrationBatchKind::ChunkEvidenceLinks)?;
        Ok(vec![(); candidates.len()])
    }

    fn fetch_evidence_units(
        &self,
        candidates: &[CandidateValidation],
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Vec<Self::EvidenceUnit>> {
        statements.record_hydration_batch(HydrationBatchKind::EvidenceUnits)?;
        Ok(vec![(); candidates.len()])
    }

    fn hydrate_full_batch(
        &self,
        candidates: &[CandidateValidation],
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Vec<FullHydration<Self::Hydrated>>> {
        self.record("hydrate_batch");
        self.fetch_chunk_headers(candidates, statements)?;
        self.fetch_chunk_bodies(candidates, statements)?;
        self.fetch_parent_links(candidates, statements)?;
        self.fetch_chunk_evidence_links(candidates, statements)?;
        self.fetch_evidence_units(candidates, statements)?;
        Ok(candidates
            .iter()
            .cloned()
            .map(|candidate| FullHydration::new(candidate, ()))
            .collect())
    }
}

impl BoundedRetrievalContract for ContractStub {
    type Report = usize;

    fn plan(
        &self,
        budget: &SearchBudget,
        filters: &RetrievalFilters,
    ) -> OverfetchResult<RetrievalPlan> {
        self.record("plan");
        RetrievalPlan::new(*budget, filters.clone())
    }

    fn execute_retrievers(&self, plan: &RetrievalPlan) -> OverfetchResult<RetrieverCandidates> {
        self.record("execute_retrievers");
        RetrieverCandidates::new(
            plan.budget(),
            vec![candidate("only", 1.0)],
            Vec::new(),
            Vec::new(),
            Vec::new(),
        )
    }

    fn fuse_truncate(
        &self,
        candidates: RetrieverCandidates,
        plan: &RetrievalPlan,
    ) -> OverfetchResult<FusedCandidates> {
        self.record("fuse_truncate");
        FusedCandidates::fuse_truncate(candidates, plan.budget())
    }

    fn validate_candidates(
        &self,
        candidates: FusedCandidates,
        plan: &RetrievalPlan,
    ) -> OverfetchResult<ValidatedCandidates> {
        self.record("validate_candidates");
        let validations = candidates
            .candidates()
            .iter()
            .cloned()
            .map(CandidateValidation::new)
            .collect::<OverfetchResult<Vec<_>>>()?;
        ValidatedCandidates::new(validations, plan.budget())
    }

    fn report(
        &self,
        _plan: &RetrievalPlan,
        hydrated: HydrationBatch<Self::Hydrated>,
        _statements: &StatementCountInstrumentation,
    ) -> OverfetchResult<Self::Report> {
        self.record("report");
        Ok(hydrated.items().len())
    }
}
