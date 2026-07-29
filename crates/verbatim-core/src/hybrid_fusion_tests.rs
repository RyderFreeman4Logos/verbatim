use super::*;

fn budget_fields() -> FusionBudgetFields {
    FusionBudgetFields {
        max_retriever_candidates: 4,
        max_fused_pool_size: 4,
        max_rerank_input_size: 4,
        max_final_hydration_list_size: 2,
        max_debug_output_size: 4,
    }
}

fn budget() -> FusionBudget {
    FusionBudget::new(budget_fields()).expect("test budget")
}

fn profile() -> FusionProfile {
    FusionProfile::new(FusionProfileFields {
        version: 1,
        strategy: FusionStrategy::WeightedScore,
        weights: vec![RetrieverWeight::new("dense".into(), 1.0).expect("test weight")],
        score_normalization: ScoreNormalizationKind::MinMax,
        rrf_constant: 60,
        candidate_limits: budget_fields(),
        explainability: ExplainabilityLevel::Full,
        accepts_reduced_explainability: false,
    })
    .expect("test profile")
}

fn dense_result() -> RetrieverResult {
    RetrieverResult::new(
        "dense".into(),
        RetrieverKind::DenseAnn,
        RetrieverGeneration::new("dense-generation".into()).expect("test generation"),
        Some(FilterIdentity::new("tenant-filter".into()).expect("test filter")),
        vec![RetrieverCandidate::new(
            "hit-1".into(),
            RawRank::new(1).expect("test rank"),
            RawScore::new(0.25, ScoreDirection::Ascending).expect("test score"),
        )
        .expect("test retriever candidate")],
        CompletenessState::ApproximateTopK,
    )
    .expect("test retriever result")
}

fn dense_candidate() -> FusionCandidate {
    FusionCandidate::new(FusionCandidateFields {
        hit_id: "hit-1".into(),
        provenance: vec![ProvenanceEntry::new(
            "dense".into(),
            RetrieverKind::DenseAnn,
            RawRank::new(1).expect("test rank"),
            RawScore::new(0.25, ScoreDirection::Ascending).expect("test score"),
        )
        .expect("test provenance")],
        inclusion_reason: InclusionReason::RankedTopK,
    })
    .expect("test fusion candidate")
}

#[test]
fn stage_output_round_trip_retains_raw_provenance_and_approximate_completeness() {
    let candidate = dense_candidate();
    let output = FusionStageOutput::new(
        profile(),
        vec![dense_result()],
        vec![candidate.clone()],
        CompletenessState::ApproximateTopK,
        &budget(),
    )
    .expect("test stage output");

    let encoded = encode_fusion_stage_output_json(&output).expect("output encodes");
    let decoded = decode_fusion_stage_output_json(&encoded).expect("output decodes");
    assert_eq!(decoded, output);
    assert_eq!(
        output.usage(),
        FusionUsage {
            retriever_candidates: 1,
            fused_pool: 1,
            ..FusionUsage::default()
        }
    );
    assert_eq!(output.completeness(), &CompletenessState::ApproximateTopK);

    let report = ExplainabilityReport::from_candidate(
        &candidate,
        vec![AppliedWeight::new("dense".into(), 1.0).expect("test applied weight")],
    )
    .expect("test explainability report");
    assert_eq!(report.hit_id(), "hit-1");
    assert_eq!(report.rows().len(), 1);
    assert_eq!(report.rows()[0].raw_rank().get(), 1);
    assert_eq!(report.rows()[0].raw_score().value(), 0.25);
    assert_eq!(
        report.rows()[0].raw_score().direction(),
        ScoreDirection::Ascending
    );
    assert_eq!(report.applied_weights()[0].weight(), 1.0);
}

#[test]
fn exhaustive_completeness_requires_an_exhaustive_retriever() {
    let scope = ExhaustiveScopeId::new("authorized-snapshot".into()).expect("test scope");
    let coverage = CoverageAccount::new(2, 1).expect("test coverage");
    let exact = CompletenessState::ExactScopeEnumerated {
        scope_id: scope.clone(),
        coverage,
    };

    let dense_exact = RetrieverResult::new(
        "dense".into(),
        RetrieverKind::DenseAnn,
        RetrieverGeneration::new("dense-generation".into()).expect("test generation"),
        None,
        vec![RetrieverCandidate::new(
            "hit-1".into(),
            RawRank::new(1).expect("test rank"),
            RawScore::new(0.25, ScoreDirection::Ascending).expect("test score"),
        )
        .expect("test retriever candidate")],
        exact.clone(),
    );
    assert!(matches!(
        dense_exact,
        Err(FusionError::CompletenessViolation {
            code: FusionDiagnosticCode::CompletenessApproximateCannotClaimExhaustive,
            ..
        })
    ));

    let exhaustive = RetrieverResult::new(
        "enumerator".into(),
        RetrieverKind::ExhaustiveEnumeration,
        RetrieverGeneration::new("snapshot-1".into()).expect("test generation"),
        None,
        vec![RetrieverCandidate::new(
            "hit-1".into(),
            RawRank::new(1).expect("test rank"),
            RawScore::new(1.0, ScoreDirection::Descending).expect("test score"),
        )
        .expect("test retriever candidate")],
        exact,
    )
    .expect("exhaustive result is valid");
    assert!(exhaustive.may_claim_exhaustive());
    assert_eq!(
        exhaustive
            .completeness()
            .scope_id()
            .expect("exhaustive result has scope")
            .as_str(),
        "authorized-snapshot"
    );
    assert_eq!(coverage.coverage_ratio(), Some(0.5));
    assert_eq!(
        CompletenessState::default(),
        CompletenessState::ApproximateTopK
    );
    assert!(!CompletenessState::default().is_global_exact());
    assert!(RetrieverKind::DenseAnn.is_approximate());
    assert!(RetrieverKind::ExhaustiveEnumeration.may_justify_completeness());
}

#[test]
fn every_supported_mode_can_construct_a_request_and_run() {
    fn assert_mode<M: FusionMode>() {
        let request = FusionRequest::<M>::new(vec![dense_result()], profile(), budget())
            .expect("test mode request");
        assert_eq!(request.retriever_results().len(), 1);
        assert_eq!(
            FusionRun::<M>::default().current_stage(),
            FusionStage::RetrieverPool
        );
    }

    assert_mode::<ExploratorySearch>();
    assert_mode::<PrecisionRetrieve>();
    assert_mode::<ContextPack>();
    assert_mode::<Exhaustive>();
}

#[test]
fn run_enforces_forward_order_and_terminal_states() {
    let mut run = FusionRun::<ContextPack>::new();
    for stage in [
        FusionStage::AuthValidation,
        FusionStage::FusionMerge,
        FusionStage::PrecedenceRules,
        FusionStage::Diversity,
        FusionStage::Rerank,
        FusionStage::FinalSelection,
        FusionStage::Hydration,
        FusionStage::Complete,
    ] {
        run.advance(stage).expect("legal stage transition");
    }
    assert_eq!(run.current_stage(), FusionStage::Complete);
    assert!(matches!(
        run.advance(FusionStage::Disabled),
        Err(FusionError::IllegalTransition { .. })
    ));

    let mut out_of_order = FusionRun::<ExploratorySearch>::new();
    assert!(matches!(
        out_of_order.advance(FusionStage::FusionMerge),
        Err(FusionError::IllegalTransition { .. })
    ));
    out_of_order
        .advance(FusionStage::AuthValidation)
        .expect("first forward transition");
    out_of_order
        .advance(FusionStage::Incomplete)
        .expect("nonterminal run may degrade");
    assert!(matches!(
        out_of_order.advance(FusionStage::Complete),
        Err(FusionError::IllegalTransition { .. })
    ));
}

#[test]
fn profile_and_budget_validation_fail_closed() {
    let invalid_profile = FusionProfile::new(FusionProfileFields {
        version: 1,
        strategy: FusionStrategy::WeightedScore,
        weights: vec![RetrieverWeight::new("dense".into(), 1.0).expect("test weight")],
        score_normalization: ScoreNormalizationKind::None,
        rrf_constant: 60,
        candidate_limits: budget_fields(),
        explainability: ExplainabilityLevel::Full,
        accepts_reduced_explainability: false,
    });
    assert!(matches!(
        invalid_profile,
        Err(FusionError::Validation {
            code: FusionDiagnosticCode::ScoreNormalizationUnsupportedForStrategy,
        })
    ));

    assert!(matches!(
        FusionUsage {
            retriever_candidates: 5,
            ..FusionUsage::default()
        }
        .check(&budget()),
        Err(FusionError::BudgetExhausted { exhaustion })
            if exhaustion.dimension == FusionBudgetDimension::RetrieverCandidates
    ));
}

#[test]
fn diagnostics_render_only_stable_codes() {
    let secret = "credential=top-secret";
    let error = FusionError::validation(FusionDiagnosticCode::RawRankMustBePositive);

    assert_eq!(
        format!("{error:?}"),
        "FusionError(raw_rank_must_be_positive)"
    );
    assert_eq!(error.to_string(), "hybrid-fusion.raw_rank_must_be_positive");
    assert!(!format!("{error:?}").contains(secret));
    assert!(!error.to_string().contains(secret));
}

fn exact_scope(scope_id: &str) -> CompletenessState {
    CompletenessState::ExactScopeEnumerated {
        scope_id: ExhaustiveScopeId::new(scope_id.into()).expect("test scope"),
        coverage: CoverageAccount::new(2, 1).expect("test coverage"),
    }
}

fn exhaustive_result(scope_id: &str) -> RetrieverResult {
    RetrieverResult::new(
        "enumerator".into(),
        RetrieverKind::ExhaustiveEnumeration,
        RetrieverGeneration::new("snapshot-1".into()).expect("test generation"),
        None,
        vec![RetrieverCandidate::new(
            "hit-1".into(),
            RawRank::new(1).expect("test rank"),
            RawScore::new(1.0, ScoreDirection::Descending).expect("test score"),
        )
        .expect("test retriever candidate")],
        exact_scope(scope_id),
    )
    .expect("test exhaustive result")
}

fn exact_candidate() -> FusionCandidate {
    FusionCandidate::new(FusionCandidateFields {
        hit_id: "hit-1".into(),
        provenance: vec![
            ProvenanceEntry::new(
                "dense".into(),
                RetrieverKind::DenseAnn,
                RawRank::new(1).expect("test rank"),
                RawScore::new(0.25, ScoreDirection::Ascending).expect("test score"),
            )
            .expect("test dense provenance"),
            ProvenanceEntry::new(
                "enumerator".into(),
                RetrieverKind::ExhaustiveEnumeration,
                RawRank::new(1).expect("test rank"),
                RawScore::new(1.0, ScoreDirection::Descending).expect("test score"),
            )
            .expect("test exhaustive provenance"),
        ],
        inclusion_reason: InclusionReason::ExhaustiveScopeMatch,
    })
    .expect("test exact fusion candidate")
}

#[test]
fn fusion_error_is_not_serde_serializable() {
    let cases = trybuild::TestCases::new();
    cases.compile_fail("tests/ui/hybrid_fusion_error_not_serializable.rs");
}

#[test]
fn stage_output_rejects_provenance_not_matching_bound_retriever() {
    fn assert_provenance_mismatch(provenance: ProvenanceEntry) {
        let mismatched = FusionCandidate::new(FusionCandidateFields {
            hit_id: "hit-1".into(),
            provenance: vec![provenance],
            inclusion_reason: InclusionReason::RankedTopK,
        })
        .expect("test fusion candidate");

        let error = FusionStageOutput::new(
            profile(),
            vec![dense_result()],
            vec![mismatched],
            CompletenessState::ApproximateTopK,
            &budget(),
        )
        .expect_err("mismatched provenance must be rejected");
        assert_eq!(
            error.diagnostic_code(),
            FusionDiagnosticCode::StageOutputProvenanceMismatch
        );
    }

    assert_provenance_mismatch(
        ProvenanceEntry::new(
            "dense".into(),
            RetrieverKind::DenseAnn,
            RawRank::new(2).expect("mismatched test rank"),
            RawScore::new(0.25, ScoreDirection::Ascending).expect("test score"),
        )
        .expect("rank-mismatched provenance"),
    );
    assert_provenance_mismatch(
        ProvenanceEntry::new(
            "dense".into(),
            RetrieverKind::DenseAnn,
            RawRank::new(1).expect("test rank"),
            RawScore::new(0.5, ScoreDirection::Ascending).expect("mismatched test score"),
        )
        .expect("score-mismatched provenance"),
    );
    assert_provenance_mismatch(
        ProvenanceEntry::new(
            "dense".into(),
            RetrieverKind::LexicalBm25,
            RawRank::new(1).expect("test rank"),
            RawScore::new(0.25, ScoreDirection::Ascending).expect("test score"),
        )
        .expect("kind-mismatched provenance"),
    );
}

#[test]
fn stage_output_exact_scope_requires_matching_exhaustive_contribution() {
    let mismatched_scope = FusionStageOutput::new(
        profile(),
        vec![dense_result(), exhaustive_result("different-snapshot")],
        vec![dense_candidate()],
        exact_scope("authorized-snapshot"),
        &budget(),
    );
    assert!(matches!(
        mismatched_scope,
        Err(FusionError::CompletenessViolation {
            code: FusionDiagnosticCode::CompletenessApproximateCannotClaimExhaustive,
            ..
        })
    ));

    let missing_contribution = FusionStageOutput::new(
        profile(),
        vec![dense_result(), exhaustive_result("authorized-snapshot")],
        vec![dense_candidate()],
        exact_scope("authorized-snapshot"),
        &budget(),
    );
    assert!(matches!(
        missing_contribution,
        Err(FusionError::CompletenessViolation {
            code: FusionDiagnosticCode::CompletenessApproximateCannotClaimExhaustive,
            ..
        })
    ));

    assert!(FusionStageOutput::new(
        profile(),
        vec![dense_result(), exhaustive_result("authorized-snapshot")],
        vec![exact_candidate()],
        exact_scope("authorized-snapshot"),
        &budget(),
    )
    .is_ok());
}

#[test]
fn decode_rejects_tampered_stage_output_usage() {
    let output = FusionStageOutput::new(
        profile(),
        vec![dense_result()],
        vec![dense_candidate()],
        CompletenessState::ApproximateTopK,
        &budget(),
    )
    .expect("test stage output");
    let encoded = encode_fusion_stage_output_json(&output).expect("test output encodes");
    let mut tampered: serde_json::Value = serde_json::from_str(&encoded).expect("valid test json");
    tampered["usage"]["fused_pool"] = serde_json::json!(0);

    let error = decode_fusion_stage_output_json(
        &serde_json::to_string(&tampered).expect("tampered json encodes"),
    )
    .expect_err("tampered usage must be rejected");
    assert_eq!(
        error.diagnostic_code(),
        FusionDiagnosticCode::StageOutputUsageMismatch
    );
}

#[test]
fn weighted_score_rejects_empty_weights() {
    assert!(matches!(
        FusionProfile::new(FusionProfileFields {
            version: 1,
            strategy: FusionStrategy::WeightedScore,
            weights: Vec::new(),
            score_normalization: ScoreNormalizationKind::MinMax,
            rrf_constant: 60,
            candidate_limits: budget_fields(),
            explainability: ExplainabilityLevel::Full,
            accepts_reduced_explainability: false,
        }),
        Err(FusionError::Validation {
            code: FusionDiagnosticCode::ProfileWeightsMustSumToUnit,
        })
    ));
}

#[test]
fn stage_output_rejects_duplicate_retriever_id() {
    let error = FusionStageOutput::new(
        profile(),
        vec![dense_result(), dense_result()],
        vec![dense_candidate()],
        CompletenessState::ApproximateTopK,
        &budget(),
    )
    .expect_err("duplicate retriever_id must be rejected");
    assert_eq!(
        error.diagnostic_code(),
        FusionDiagnosticCode::StageOutputDuplicateRetrieverId
    );
}

#[test]
fn decode_rejects_invalid_completeness_coverage() {
    let output = FusionStageOutput::new(
        profile(),
        vec![dense_result(), exhaustive_result("authorized-snapshot")],
        vec![exact_candidate()],
        exact_scope("authorized-snapshot"),
        &budget(),
    )
    .expect("valid test output");
    let encoded = encode_fusion_stage_output_json(&output).expect("valid output encodes");
    let mut tampered: serde_json::Value = serde_json::from_str(&encoded).expect("valid test json");
    tampered["completeness"]["coverage"]["enumerated"] = serde_json::json!(0);

    let error = decode_fusion_stage_output_json(
        &serde_json::to_string(&tampered).expect("tampered json encodes"),
    )
    .expect_err("invalid coverage must be rejected");
    assert_eq!(
        error.diagnostic_code(),
        FusionDiagnosticCode::InvalidStageOutputJson
    );
}

#[test]
fn direct_serde_deserialize_rejects_invalid_coverage() {
    let output = FusionStageOutput::new(
        profile(),
        vec![dense_result(), exhaustive_result("authorized-snapshot")],
        vec![exact_candidate()],
        exact_scope("authorized-snapshot"),
        &budget(),
    )
    .expect("valid test output");
    let encoded = encode_fusion_stage_output_json(&output).expect("valid output encodes");
    let mut tampered: serde_json::Value = serde_json::from_str(&encoded).expect("valid test json");
    tampered["completeness"]["coverage"]["enumerated"] = serde_json::json!(0);

    let result: Result<FusionStageOutput, _> =
        serde_json::from_str(&serde_json::to_string(&tampered).expect("tampered json encodes"));
    assert!(
        result.is_err(),
        "direct serde must reject invalid coverage via custom deserializer"
    );
}

#[test]
fn output_coverage_must_match_qualifying_exhaustive_retriever() {
    let mismatched_coverage = CompletenessState::ExactScopeEnumerated {
        scope_id: ExhaustiveScopeId::new("authorized-snapshot".into()).expect("test scope"),
        coverage: CoverageAccount::new(99, 99).expect("different coverage"),
    };
    let error = FusionStageOutput::new(
        profile(),
        vec![dense_result(), exhaustive_result("authorized-snapshot")],
        vec![exact_candidate()],
        mismatched_coverage,
        &budget(),
    )
    .expect_err("coverage mismatch must be rejected");
    assert_eq!(
        error.diagnostic_code(),
        FusionDiagnosticCode::CompletenessApproximateCannotClaimExhaustive
    );
}
