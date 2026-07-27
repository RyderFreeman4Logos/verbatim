use std::cell::RefCell;

use super::super::contract::{
    sealed, BoundedRetrievalContract, FusedCandidates, PrimaryBackendAttempt, RetrievalAdapter,
    RetrievalPlan, RetrieverCandidates,
};
use super::super::count::CountPort;
use super::super::hydration::{BatchHydrationPort, FullHydration, HydrationBatch};
use super::super::policy::{CandidateValidation, ValidatedCandidates};
use super::{budget, candidate};
use crate::overfetch::{
    AdaptiveOverfetchPolicy, AdaptiveOverfetchPolicyFields, CandidateId, HydrationBatchKind,
    OverfetchError, OverfetchResult, PrimaryBackendSelection, RetrievalBackend, RetrievalCandidate,
    RetrievalFilters, SearchBudget, SearchBudgetFields, StatementCountInstrumentation,
    StrictFilter, StrictFilterSupport, TypedBackendFailure,
};

fn primary_backend_selection() -> PrimaryBackendSelection {
    PrimaryBackendSelection::new(RetrievalBackend::Qdrant, Some(RetrievalBackend::LocalDense))
        .expect("distinct primary and fallback")
}

fn strict_filters() -> RetrievalFilters {
    RetrievalFilters::new(vec![
        StrictFilter::source("source-a").expect("source filter"),
        StrictFilter::collection("collection-a").expect("collection filter"),
        StrictFilter::tenant("tenant-a").expect("tenant filter"),
        StrictFilter::acl("acl-a").expect("ACL filter"),
    ])
    .expect("bounded strict filters")
}

fn retrieve(
    contract: &ContractStub,
    filters: &RetrievalFilters,
    support: &StrictFilterSupport,
    statements: &mut StatementCountInstrumentation,
) -> OverfetchResult<usize> {
    contract.retrieve(
        &budget(),
        filters,
        support,
        &primary_backend_selection(),
        statements,
    )
}

#[test]
fn overfetch_contract_retrieve_runs_one_atomic_primary_first_pipeline() {
    let contract = ContractStub::default();
    let support = StrictFilterSupport::Native;
    let mut statements = StatementCountInstrumentation::new(5).expect("statement cap");

    assert_eq!(
        retrieve(
            &contract,
            &RetrievalFilters::default(),
            &support,
            &mut statements,
        )
        .expect("bounded orchestration succeeds"),
        1
    );
    assert_eq!(
        contract.calls.borrow().as_slice(),
        [
            "plan",
            "count_indexed",
            "execute:qdrant",
            "fuse_truncate",
            "validate_candidates",
            "hydrate_batch",
            "report",
        ]
    );
    assert_eq!(
        *contract.observed_support.borrow(),
        Some(StrictFilterSupport::Native)
    );
    assert_eq!(statements.observed_statements(), 5);
}

#[test]
fn overfetch_contract_retrieve_uses_fallback_only_after_typed_primary_failure() {
    let contract = ContractStub::configured(
        1_000,
        &[
            Attempt::TypedFailure(TypedBackendFailure::Unavailable),
            Attempt::Satisfied,
        ],
        PlanTampering::None,
        HydrationMode::Complete,
    );
    let mut statements = StatementCountInstrumentation::new(5).expect("statement cap");

    assert_eq!(
        retrieve(
            &contract,
            &RetrievalFilters::default(),
            &StrictFilterSupport::Native,
            &mut statements,
        )
        .expect("typed primary failure may use the declared fallback"),
        1
    );
    assert_eq!(
        contract.calls.borrow().as_slice(),
        [
            "plan",
            "count_indexed",
            "execute:qdrant",
            "execute:local_dense",
            "fuse_truncate",
            "validate_candidates",
            "hydrate_batch",
            "report",
        ]
    );
}

#[test]
fn overfetch_contract_retrieve_fails_closed_after_declared_primary_insufficiency() {
    let contract = ContractStub::configured(
        1_000,
        &[Attempt::DeclaredInsufficientResults, Attempt::Satisfied],
        PlanTampering::None,
        HydrationMode::Complete,
    );
    let mut statements = StatementCountInstrumentation::new(5).expect("statement cap");

    assert_eq!(
        retrieve(
            &contract,
            &RetrievalFilters::default(),
            &StrictFilterSupport::Native,
            &mut statements,
        )
        .expect_err("declared insufficiency may not invoke fallback"),
        OverfetchError::PrimaryBackendRequired
    );
    assert_eq!(
        contract.calls.borrow().as_slice(),
        ["plan", "count_indexed", "execute:qdrant"]
    );
}

#[test]
fn overfetch_contract_retrieve_gates_strict_filters_and_corpus_top_k_before_retrievers() {
    let contract = ContractStub::configured(
        4,
        &[Attempt::Satisfied],
        PlanTampering::None,
        HydrationMode::Complete,
    );
    let filters = strict_filters();
    let mut statements = StatementCountInstrumentation::new(5).expect("statement cap");

    assert_eq!(
        retrieve(
            &contract,
            &filters,
            &StrictFilterSupport::Native,
            &mut statements,
        )
        .expect_err("native strict top-k cannot equal the corpus size"),
        OverfetchError::CorpusSizeTopKForbidden
    );
    assert_eq!(
        contract.calls.borrow().as_slice(),
        ["plan", "count_indexed"]
    );
}

#[test]
fn overfetch_contract_retrieve_rejects_unsupported_strict_filters_before_retrievers() {
    let contract = ContractStub::default();
    let filters = strict_filters();
    let mut statements = StatementCountInstrumentation::new(5).expect("statement cap");

    assert_eq!(
        retrieve(
            &contract,
            &filters,
            &StrictFilterSupport::Unsupported,
            &mut statements,
        )
        .expect_err("strict filtering must fail closed when unsupported"),
        OverfetchError::UnsupportedStrictFilter
    );
    assert_eq!(
        contract.calls.borrow().as_slice(),
        ["plan", "count_indexed"]
    );
}

#[test]
fn overfetch_contract_retrieve_passes_counted_adaptive_caps_to_the_retriever() {
    let policy = AdaptiveOverfetchPolicy::new(AdaptiveOverfetchPolicyFields {
        initial_candidate_k: 3,
        max_candidate_k: 4,
        growth_factor: 2,
        max_attempts: 2,
    })
    .expect("adaptive policy");
    let support = StrictFilterSupport::Adaptive(policy);
    let contract = ContractStub::default();
    let filters = strict_filters();
    let mut statements = StatementCountInstrumentation::new(5).expect("statement cap");

    retrieve(&contract, &filters, &support, &mut statements)
        .expect("bounded adaptive strict retrieval succeeds");

    assert_eq!(*contract.observed_support.borrow(), Some(support));
    assert_eq!(*contract.observed_dense_candidate_k.borrow(), Some(3));
}

#[test]
fn overfetch_contract_retrieve_rejects_widened_budgets_and_dropped_strict_predicates() {
    let widened = ContractStub::configured(
        1_000,
        &[Attempt::Satisfied],
        PlanTampering::WidenBudget,
        HydrationMode::Complete,
    );
    let mut statements = StatementCountInstrumentation::new(5).expect("statement cap");
    assert_eq!(
        retrieve(
            &widened,
            &RetrievalFilters::default(),
            &StrictFilterSupport::Native,
            &mut statements,
        )
        .expect_err("a planner must not widen the caller budget"),
        OverfetchError::BudgetExceeded
    );
    assert_eq!(widened.calls.borrow().as_slice(), ["plan"]);

    let dropped = ContractStub::configured(
        1_000,
        &[Attempt::Satisfied],
        PlanTampering::DropFilters,
        HydrationMode::Complete,
    );
    let mut statements = StatementCountInstrumentation::new(5).expect("statement cap");
    assert_eq!(
        retrieve(
            &dropped,
            &strict_filters(),
            &StrictFilterSupport::Native,
            &mut statements,
        )
        .expect_err("a planner must preserve ACL, tenant, collection, and source filters"),
        OverfetchError::UnsupportedStrictFilter
    );
    assert_eq!(dropped.calls.borrow().as_slice(), ["plan"]);
}

#[test]
fn overfetch_contract_retrieve_rejects_omitted_and_unrecorded_hydration_batches() {
    for mode in [HydrationMode::OmitEvidenceUnits, HydrationMode::Unrecorded] {
        let contract =
            ContractStub::configured(1_000, &[Attempt::Satisfied], PlanTampering::None, mode);
        let mut statements = StatementCountInstrumentation::new(6).expect("statement cap");

        assert_eq!(
            retrieve(
                &contract,
                &RetrievalFilters::default(),
                &StrictFilterSupport::Native,
                &mut statements,
            )
            .expect_err("retrieve must enforce complete hydration instrumentation"),
            OverfetchError::NPlusOneDetected
        );
        assert!(contract.calls.borrow().contains(&"hydrate_batch"));
        assert!(!contract.calls.borrow().contains(&"report"));
    }
}

#[test]
fn overfetch_contract_retrieve_rejects_extra_per_candidate_sql_with_spare_capacity() {
    let contract = ContractStub::configured(
        1_000,
        &[Attempt::Satisfied],
        PlanTampering::None,
        HydrationMode::PerCandidate,
    );
    let mut statements = StatementCountInstrumentation::new(6).expect("statement cap");

    assert_eq!(
        retrieve(
            &contract,
            &RetrievalFilters::default(),
            &StrictFilterSupport::Native,
            &mut statements,
        )
        .expect_err("a broad statement cap cannot hide per-candidate reads"),
        OverfetchError::NPlusOneDetected
    );
    assert_eq!(statements.observed_statements(), 6);
}

#[test]
fn overfetch_contract_domain_debug_redacts_filter_and_identifier_values() {
    let source = "source-secret";
    let collection = "collection-secret";
    let tenant = "tenant-secret";
    let acl = "acl-secret";
    let candidate_id = "candidate-secret";
    let source_filter = StrictFilter::source(source).expect("source filter");
    let collection_filter = StrictFilter::collection(collection).expect("collection filter");
    let tenant_filter = StrictFilter::tenant(tenant).expect("tenant filter");
    let acl_filter = StrictFilter::acl(acl).expect("ACL filter");
    let filters = RetrievalFilters::new(vec![
        source_filter.clone(),
        collection_filter.clone(),
        tenant_filter.clone(),
        acl_filter.clone(),
    ])
    .expect("bounded filters");
    let id = CandidateId::new(candidate_id).expect("candidate identifier");
    let retrieval_candidate =
        RetrievalCandidate::new(id.clone(), 1.0).expect("finite candidate score");
    let plan = RetrievalPlan::new(
        budget(),
        filters.clone(),
        StrictFilterSupport::Native,
        primary_backend_selection(),
    )
    .expect("internal plan");

    let rendered = [
        format!("{source_filter:?}"),
        format!("{collection_filter:?}"),
        format!("{tenant_filter:?}"),
        format!("{acl_filter:?}"),
        format!("{filters:?}"),
        format!("{id:?}"),
        format!("{retrieval_candidate:?}"),
        format!("{plan:?}"),
    ]
    .join("\n");
    for secret in [source, collection, tenant, acl, candidate_id] {
        assert!(
            !rendered.contains(secret),
            "domain Debug must redact caller-controlled values"
        );
    }
}

#[derive(Clone, Copy)]
enum Attempt {
    Satisfied,
    DeclaredInsufficientResults,
    TypedFailure(TypedBackendFailure),
}

#[derive(Clone, Copy)]
enum PlanTampering {
    None,
    WidenBudget,
    DropFilters,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum HydrationMode {
    Complete,
    OmitEvidenceUnits,
    Unrecorded,
    PerCandidate,
}

struct ContractStub {
    calls: RefCell<Vec<&'static str>>,
    corpus_size: u64,
    attempts: RefCell<Vec<Attempt>>,
    plan_tampering: PlanTampering,
    hydration_mode: HydrationMode,
    observed_support: RefCell<Option<StrictFilterSupport>>,
    observed_dense_candidate_k: RefCell<Option<u32>>,
}

impl Default for ContractStub {
    fn default() -> Self {
        Self::configured(
            1_000,
            &[Attempt::Satisfied],
            PlanTampering::None,
            HydrationMode::Complete,
        )
    }
}

impl ContractStub {
    fn configured(
        corpus_size: u64,
        attempts: &[Attempt],
        plan_tampering: PlanTampering,
        hydration_mode: HydrationMode,
    ) -> Self {
        Self {
            calls: RefCell::new(Vec::new()),
            corpus_size,
            attempts: RefCell::new(attempts.to_vec()),
            plan_tampering,
            hydration_mode,
            observed_support: RefCell::new(None),
            observed_dense_candidate_k: RefCell::new(None),
        }
    }

    fn record(&self, call: &'static str) {
        self.calls.borrow_mut().push(call);
    }

    fn record_hydration_batch(
        &self,
        batch: HydrationBatchKind,
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<()> {
        if self.hydration_mode == HydrationMode::OmitEvidenceUnits
            && batch == HydrationBatchKind::EvidenceUnits
        {
            return Ok(());
        }
        statements.record_hydration_batch(batch)
    }
}

impl sealed::Sealed for ContractStub {}

impl CountPort for ContractStub {
    fn count_indexed(&self, _filters: &RetrievalFilters) -> OverfetchResult<u64> {
        self.record("count_indexed");
        Ok(self.corpus_size)
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
        self.record_hydration_batch(HydrationBatchKind::ChunkHeaders, statements)?;
        Ok(vec![(); candidates.len()])
    }

    fn fetch_chunk_bodies(
        &self,
        candidates: &[CandidateValidation],
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Vec<Self::ChunkBody>> {
        self.record_hydration_batch(HydrationBatchKind::ChunkBodies, statements)?;
        Ok(vec![(); candidates.len()])
    }

    fn fetch_parent_links(
        &self,
        candidates: &[CandidateValidation],
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Vec<Self::ParentLink>> {
        self.record_hydration_batch(HydrationBatchKind::ParentLinks, statements)?;
        Ok(vec![(); candidates.len()])
    }

    fn fetch_chunk_evidence_links(
        &self,
        candidates: &[CandidateValidation],
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Vec<Self::ChunkEvidenceLink>> {
        self.record_hydration_batch(HydrationBatchKind::ChunkEvidenceLinks, statements)?;
        Ok(vec![(); candidates.len()])
    }

    fn fetch_evidence_units(
        &self,
        candidates: &[CandidateValidation],
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Vec<Self::EvidenceUnit>> {
        self.record_hydration_batch(HydrationBatchKind::EvidenceUnits, statements)?;
        Ok(vec![(); candidates.len()])
    }

    fn hydrate_full_batch(
        &self,
        candidates: &[CandidateValidation],
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Vec<FullHydration<Self::Hydrated>>> {
        self.record("hydrate_batch");
        match self.hydration_mode {
            HydrationMode::Unrecorded => {
                for _ in candidates {
                    statements.record_statement()?;
                }
            }
            HydrationMode::Complete
            | HydrationMode::OmitEvidenceUnits
            | HydrationMode::PerCandidate => {
                self.fetch_chunk_headers(candidates, statements)?;
                self.fetch_chunk_bodies(candidates, statements)?;
                self.fetch_parent_links(candidates, statements)?;
                self.fetch_chunk_evidence_links(candidates, statements)?;
                self.fetch_evidence_units(candidates, statements)?;
                if self.hydration_mode == HydrationMode::PerCandidate {
                    for _ in candidates {
                        statements.record_statement()?;
                    }
                }
            }
        }
        Ok(candidates
            .iter()
            .cloned()
            .map(|candidate| FullHydration::new(candidate, ()))
            .collect())
    }
}

impl RetrievalAdapter for ContractStub {
    type Report = usize;

    fn plan(
        &self,
        budget: &SearchBudget,
        filters: &RetrievalFilters,
        strict_filter_support: &StrictFilterSupport,
        primary_backend_selection: &PrimaryBackendSelection,
    ) -> OverfetchResult<RetrievalPlan> {
        self.record("plan");
        match self.plan_tampering {
            PlanTampering::None => RetrievalPlan::new(
                *budget,
                filters.clone(),
                strict_filter_support.clone(),
                *primary_backend_selection,
            ),
            PlanTampering::WidenBudget => {
                let widened_budget = SearchBudget::new(SearchBudgetFields {
                    dense_candidate_k: budget.dense_candidate_k + 1,
                    lexical_candidate_k: budget.lexical_candidate_k,
                    exact_candidate_k: budget.exact_candidate_k,
                    graph_candidate_k: budget.graph_candidate_k,
                    fused_pool_size: budget.fused_pool_size,
                    rerank_input_size: budget.rerank_input_size,
                    final_hydration_list_size: budget.final_hydration_list_size,
                    debug_output_size: budget.debug_output_size,
                })?;
                RetrievalPlan::new(
                    widened_budget,
                    filters.clone(),
                    strict_filter_support.clone(),
                    *primary_backend_selection,
                )
            }
            PlanTampering::DropFilters => RetrievalPlan::new(
                *budget,
                RetrievalFilters::default(),
                strict_filter_support.clone(),
                *primary_backend_selection,
            ),
        }
    }

    fn execute_retrievers(
        &self,
        plan: &RetrievalPlan,
        backend: RetrievalBackend,
    ) -> OverfetchResult<PrimaryBackendAttempt> {
        *self.observed_support.borrow_mut() = Some(plan.strict_filter_support().clone());
        *self.observed_dense_candidate_k.borrow_mut() = Some(plan.budget().dense_candidate_k);
        self.record(match backend {
            RetrievalBackend::Qdrant => "execute:qdrant",
            RetrievalBackend::LocalDense => "execute:local_dense",
            RetrievalBackend::DiskAnn3 | RetrievalBackend::LanceDb => "execute:other",
        });
        let attempt = self.attempts.borrow_mut().remove(0);
        match attempt {
            Attempt::Satisfied => Ok(PrimaryBackendAttempt::Satisfied(RetrieverCandidates::new(
                plan.budget(),
                vec![candidate("only", 1.0)],
                Vec::new(),
                Vec::new(),
                Vec::new(),
            )?)),
            Attempt::DeclaredInsufficientResults => {
                Ok(PrimaryBackendAttempt::DeclaredInsufficientResults)
            }
            Attempt::TypedFailure(failure) => Ok(PrimaryBackendAttempt::TypedFailure(failure)),
        }
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
