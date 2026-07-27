use crate::diskann3::*;

fn budget() -> SearchBudget {
    SearchBudget::new(
        MemoryBudget::new(32 * MEBIBYTE, 128 * MEBIBYTE).expect("valid memory budget"),
        IoBudget::new(64, 8 * MEBIBYTE, 250).expect("valid I/O budget"),
        ConcurrencyBudget::new(2, 16).expect("valid concurrency budget"),
        RetrievalStageBudget::uniform(32).expect("valid stage budget"),
    )
    .expect("valid combined search budget")
}

fn candidate(id: &str, generation: PublicationGeneration) -> VectorCandidate {
    VectorCandidate::new(id, 0.5, generation, false).expect("valid candidate")
}

#[test]
fn full_dimension_only_accepts_finite_4096_float32_vectors() {
    let vector = vec![0.25_f32; VectorDimension::FULL_PRECISION_F32];
    assert_eq!(
        VectorDimension::new(VectorDimension::FULL_PRECISION_F32).expect("4096 is supported"),
        VectorDimension::FULL_PRECISION
    );
    VectorDimension::validate_vector(&vector).expect("full vector validates");

    for invalid in [
        vec![0.0_f32; VectorDimension::FULL_PRECISION_F32 - 1],
        vec![f32::NAN; VectorDimension::FULL_PRECISION_F32],
        vec![f32::INFINITY; VectorDimension::FULL_PRECISION_F32],
    ] {
        assert_eq!(
            VectorDimension::validate_vector(&invalid)
                .expect_err("non-4096 or non-finite vectors fail closed")
                .diagnostic_code(),
            VectorSearchDiagnosticCode::DimensionMismatch
        );
    }

    assert!(
        serde_json::from_str::<VectorDimension>("1").is_err(),
        "serialized dimensions must preserve the 4096-dimensional invariant"
    );
}

#[test]
fn vector_space_generation_shard_and_manifest_are_bounded_and_round_trip() {
    let space = VectorSpaceId::new("text-english-v1").expect("valid named vector space");
    let generation = PublicationGeneration::new(7).expect("nonzero publication generation");
    let shard = ShardId::new(space.clone(), generation, 3).expect("bounded shard ID");
    let manifest = SsdShardManifest::new(SsdShardManifestFields {
        shard,
        vector_count: 12_000,
        dimension: VectorDimension::FULL_PRECISION,
        byte_size: 12_000 * 4_096 * 4,
        graph_degree: 64,
        quantizer: QuantizerType::ProductQuantizedCandidate,
        page_layout: SsdPageLayout::AisaqCoLocated,
        checksum: ShardChecksum::new(
            "sha256:0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        )
        .expect("checksum"),
    })
    .expect("valid SSD manifest");

    let json = encode_ssd_shard_manifest_json(&manifest).expect("manifest encodes");
    assert_eq!(
        decode_ssd_shard_manifest_json(&json).expect("manifest decodes"),
        manifest
    );
}

#[test]
fn invalid_shard_manifest_and_untrusted_json_expose_only_closed_codes() {
    let space = VectorSpaceId::new("text").expect("space");
    let generation = PublicationGeneration::new(1).expect("generation");
    let shard = ShardId::new(space, generation, 0).expect("shard");
    let error = SsdShardManifest::new(SsdShardManifestFields {
        shard,
        vector_count: 1,
        dimension: VectorDimension::FULL_PRECISION,
        byte_size: 1,
        graph_degree: 0,
        quantizer: QuantizerType::None,
        page_layout: SsdPageLayout::SeparateGraphAndVectors,
        checksum: ShardChecksum::new("bad").expect("syntax is nonempty"),
    })
    .expect_err("undersized bytes and zero graph degree fail closed");
    assert_eq!(
        error.diagnostic_code(),
        VectorSearchDiagnosticCode::ShardCorrupt
    );

    let untrusted = "token=not-for-logs";
    let error = decode_ssd_shard_manifest_json(untrusted).expect_err("malformed JSON fails");
    assert_eq!(
        error.diagnostic_code(),
        VectorSearchDiagnosticCode::InvalidManifest
    );
    assert!(!format!("{error:?}").contains(untrusted));
    assert!(!error.to_string().contains(untrusted));

    let truncated_checksum =
        ShardChecksum::new("sha256:0123456789abcdef").expect("bounded checksum syntax");
    let shard = ShardId::new(
        VectorSpaceId::new("text").expect("space"),
        PublicationGeneration::new(1).expect("generation"),
        0,
    )
    .expect("shard");
    assert!(
        SsdShardManifest::new(SsdShardManifestFields {
            shard,
            vector_count: 1,
            dimension: VectorDimension::FULL_PRECISION,
            byte_size: 4_096 * 4,
            graph_degree: 1,
            quantizer: QuantizerType::None,
            page_layout: SsdPageLayout::SeparateGraphAndVectors,
            checksum: truncated_checksum,
        })
        .is_err(),
        "a sha256 checksum cannot be truncated"
    );
}

#[test]
fn every_backend_and_role_is_reachable_and_legacy_is_explicitly_gated() {
    let expected = [
        (VectorBackend::DiskAnn3, BackendRole::Primary),
        (VectorBackend::Qdrant, BackendRole::Reference),
        (VectorBackend::LanceDb, BackendRole::Reference),
        (VectorBackend::SQLite, BackendRole::Legacy),
        (VectorBackend::HnswLegacy, BackendRole::Legacy),
    ];
    assert_eq!(expected.len(), VectorBackend::ALL.len());
    assert_eq!(BackendRole::ALL.len(), 3);

    for (backend, role) in expected {
        assert_eq!(backend.role(), role);
        let selection = BackendSelection::new(backend, false);
        if role == BackendRole::Legacy {
            assert_eq!(
                selection
                    .expect_err("legacy scans need explicit opt-in")
                    .diagnostic_code(),
                VectorSearchDiagnosticCode::LegacyBackendOptInRequired
            );
            BackendSelection::new(backend, true).expect("explicit legacy opt-in");
        } else {
            selection.expect("non-legacy backend selection");
        }
    }

    assert!(
        serde_json::from_str::<BackendSelection>(
            r#"{"backend":"s_q_lite","legacy_opt_in":false}"#,
        )
        .is_err(),
        "deserialization must not bypass legacy operator opt-in"
    );
}

#[test]
fn resource_budgets_enforce_memory_io_and_concurrency_fail_closed() {
    assert_eq!(
        MemoryBudget::new(129 * MEBIBYTE, 128 * MEBIBYTE)
            .expect_err("request memory cannot exceed global hard cap")
            .diagnostic_code(),
        VectorSearchDiagnosticCode::BudgetExceeded
    );
    assert_eq!(
        MemoryBudget::new(64 * MEBIBYTE, MAX_PEAK_MEMORY_BYTES + 1)
            .expect_err("memory must remain under the architecture cap")
            .diagnostic_code(),
        VectorSearchDiagnosticCode::BudgetExceeded
    );
    assert_eq!(
        IoBudget::new(0, 1, 1)
            .expect_err("zero page reads cannot permit search")
            .diagnostic_code(),
        VectorSearchDiagnosticCode::BudgetExceeded
    );
    assert_eq!(
        ConcurrencyBudget::new(9, 8)
            .expect_err("request concurrency cannot exceed global cap")
            .diagnostic_code(),
        VectorSearchDiagnosticCode::BudgetExceeded
    );

    let error = budget()
        .check_usage(BudgetUsage {
            peak_memory_bytes: 33 * MEBIBYTE,
            page_reads: 1,
            bytes_read: 1,
            elapsed_ms: 1,
            request_concurrency: 1,
            global_concurrency: 1,
        })
        .expect_err("usage exceeding a cap fails closed");
    assert_eq!(
        error.diagnostic_code(),
        VectorSearchDiagnosticCode::BudgetExceeded
    );
}

#[test]
fn all_retrieval_stages_have_hard_output_caps_and_required_transitions() {
    assert_eq!(RetrievalStage::ALL.len(), 6);
    let stage_budget = RetrievalStageBudget::new([3, 3, 3, 2, 2, 2]).expect("stage caps");
    let generation = PublicationGeneration::new(3).expect("generation");
    let candidates = vec![
        candidate("a", generation),
        candidate("b", generation),
        candidate("c", generation),
    ];
    assert_eq!(
        BoundedCandidates::new(
            [
                candidate("a", generation),
                candidate("b", generation),
                candidate("c", generation),
                candidate("d", generation),
            ]
            .to_vec(),
            generation,
            &stage_budget,
        )
        .expect_err("candidate generation cannot overrun its stage cap")
        .diagnostic_code(),
        VectorSearchDiagnosticCode::StageOutputExceeded
    );

    let generated = BoundedCandidates::new(candidates, generation, &stage_budget)
        .expect("candidate generation");
    assert_eq!(generated.stage(), RetrievalStage::CandidateGeneration);
    assert_eq!(generated.candidates().len(), 3);
    let rescored = generated.rescore(&stage_budget).expect("rescore");
    assert_eq!(rescored.stage(), RetrievalStage::FullPrecisionRescore);
    assert_eq!(rescored.candidates().len(), 3);
    let filtered = rescored
        .apply_filters(&stage_budget)
        .expect("filter application");
    assert_eq!(filtered.stage(), RetrievalStage::FilterApplication);
    assert_eq!(filtered.candidates().len(), 3);
    let fused = filtered.fuse(&stage_budget).expect("fusion truncates");
    assert_eq!(fused.stage(), RetrievalStage::Fusion);
    assert_eq!(fused.candidates().len(), 2);
    let reranked = fused.rerank(&stage_budget).expect("rerank");
    assert_eq!(reranked.stage(), RetrievalStage::Rerank);
    assert_eq!(reranked.candidates().len(), 2);
    let hydrated = reranked.hydrate(&stage_budget).expect("hydration");
    assert_eq!(hydrated.stage(), RetrievalStage::Hydration);
    assert_eq!(hydrated.candidates().len(), 2);
}

#[test]
fn strict_small_filtered_candidate_sets_select_exact_simd_scan() {
    let threshold = ExactScanThreshold::new(16).expect("positive threshold");
    assert_eq!(
        threshold.choose_path(16, true),
        CandidateGenerationPath::ExactSimdScan
    );
    assert_eq!(
        threshold.choose_path(17, true),
        CandidateGenerationPath::AnnTraversal
    );
    assert_eq!(
        ExactScanThreshold::new(0)
            .expect_err("zero threshold silently disables exact scan")
            .diagnostic_code(),
        VectorSearchDiagnosticCode::FilterUnsupported
    );
}

#[test]
fn non_strict_small_candidate_sets_do_not_select_exact_simd_scan() {
    let threshold = ExactScanThreshold::new(16).expect("positive threshold");
    assert_eq!(
        threshold.choose_path(16, false),
        CandidateGenerationPath::AnnTraversal,
        "only a strict filter may select the exact SIMD path"
    );
}

#[test]
fn filters_cover_enterprise_scope_and_reject_unsupported_or_unbounded_metadata() {
    let filters = vec![
        FilterPredicate::source("source-a").expect("source filter"),
        FilterPredicate::collection("collection-a").expect("collection filter"),
        FilterPredicate::tenant("tenant-a").expect("tenant filter"),
        FilterPredicate::acl("group:readers").expect("ACL filter"),
        FilterPredicate::lifecycle(LifecycleState::Active),
        FilterPredicate::language("en").expect("language filter"),
        FilterPredicate::date_range(1, 2).expect("date filter"),
        FilterPredicate::metadata_eq(
            "department",
            TypedMetadataValue::String("research".to_owned()),
        )
        .expect("typed metadata filter"),
    ];
    for filter in &filters {
        filter.validate().expect("enterprise filter validates");
    }
    assert_eq!(
        FilterPredicate::date_range(2, 1)
            .expect_err("reversed date range must fail closed")
            .diagnostic_code(),
        VectorSearchDiagnosticCode::FilterUnsupported
    );
}

#[test]
fn policy_rejects_generation_mixing_tombstones_and_unbounded_filter_overfetch() {
    let policy = VectorSearchPolicy::default();
    let query = vec![0.0_f32; VectorDimension::FULL_PRECISION_F32];
    policy
        .validate_search(
            &query,
            &budget(),
            &[FilterPredicate::tenant("t").expect("filter")],
        )
        .expect("bounded filtered request validates");

    let expected = PublicationGeneration::new(2).expect("generation");
    let actual = PublicationGeneration::new(3).expect("generation");
    let error = policy
        .validate_generation(expected, actual)
        .expect_err("mixed generations fail closed");
    assert_eq!(
        error.diagnostic_code(),
        VectorSearchDiagnosticCode::GenerationMismatch
    );

    assert_eq!(
        VectorCandidate::new("deleted", 0.1, expected, true)
            .expect_err("tombstoned vectors never enter results")
            .diagnostic_code(),
        VectorSearchDiagnosticCode::TombstonedVector
    );

    assert_eq!(
        policy
            .validate_filter_selectivity(100, 100, true)
            .expect_err("strict filters cannot overfetch the full corpus")
            .diagnostic_code(),
        VectorSearchDiagnosticCode::FilterUnsupported
    );
}

#[derive(Default)]
struct ContractStub {
    policy: VectorSearchPolicy,
}

impl VectorSearchContract for ContractStub {
    fn search(
        &self,
        query: &[f32],
        budget: &SearchBudget,
        filters: &[FilterPredicate],
    ) -> VectorSearchResult<GeneratedCandidates> {
        self.policy.validate_search(query, budget, filters)?;
        let generation = PublicationGeneration::new(1)?;
        BoundedCandidates::new(
            vec![candidate("one", generation)],
            generation,
            budget.stage_budget(),
        )
    }

    fn rescore(
        &self,
        candidates: GeneratedCandidates,
        budget: &SearchBudget,
    ) -> VectorSearchResult<RescoredCandidates> {
        candidates.rescore(budget.stage_budget())
    }

    fn apply_filters(
        &self,
        candidates: RescoredCandidates,
        budget: &SearchBudget,
    ) -> VectorSearchResult<FilteredCandidates> {
        candidates.apply_filters(budget.stage_budget())
    }

    fn fuse(
        &self,
        candidates: FilteredCandidates,
        budget: &SearchBudget,
    ) -> VectorSearchResult<FusedCandidates> {
        candidates.fuse(budget.stage_budget())
    }

    fn rerank(
        &self,
        candidates: FusedCandidates,
        budget: &SearchBudget,
    ) -> VectorSearchResult<RerankedCandidates> {
        candidates.rerank(budget.stage_budget())
    }

    fn hydrate(
        &self,
        candidates: RerankedCandidates,
        budget: &SearchBudget,
    ) -> VectorSearchResult<HydratedCandidates> {
        candidates.hydrate(budget.stage_budget())
    }
}

#[test]
fn budget_search_candidates_and_serialization_round_trip_through_contract() {
    let contract = ContractStub::default();
    let query = vec![0.0_f32; VectorDimension::FULL_PRECISION_F32];
    let search_budget = budget();
    let generated = contract
        .search(
            &query,
            &search_budget,
            &[FilterPredicate::source("s").expect("filter")],
        )
        .expect("contract search");
    let rescored = contract
        .rescore(generated, &search_budget)
        .expect("rescore");
    let filtered = contract
        .apply_filters(rescored, &search_budget)
        .expect("filter application");
    let fused = contract.fuse(filtered, &search_budget).expect("fusion");
    let reranked = contract.rerank(fused, &search_budget).expect("rerank");
    let hydrated = contract.hydrate(reranked, &search_budget).expect("hydrate");
    assert_eq!(hydrated.stage(), RetrievalStage::Hydration);

    let budget_json = encode_search_budget_json(&search_budget).expect("budget encodes");
    assert_eq!(
        decode_search_budget_json(&budget_json).expect("budget decodes"),
        search_budget
    );
    let candidates_json = encode_bounded_candidates_json(&hydrated).expect("candidates encode");
    assert_eq!(
        decode_bounded_candidates_json(&candidates_json, search_budget.stage_budget())
            .expect("candidates decode"),
        hydrated
    );
}

#[test]
fn retrieval_pipeline_requires_filter_fusion_and_rerank_before_hydration() {
    let stage_budget = RetrievalStageBudget::uniform(2).expect("stage caps");
    let generation = PublicationGeneration::new(1).expect("generation");
    let hydrated = BoundedCandidates::new(
        vec![candidate("one", generation)],
        generation,
        &stage_budget,
    )
    .expect("candidate generation")
    .rescore(&stage_budget)
    .expect("rescore");
    let filtered = hydrated
        .apply_filters(&stage_budget)
        .expect("filter application");
    let fused = filtered.fuse(&stage_budget).expect("fusion");
    let reranked = fused.rerank(&stage_budget).expect("rerank");
    let hydrated = reranked.hydrate(&stage_budget).expect("hydration");

    assert_eq!(hydrated.stage(), RetrievalStage::Hydration);
}
