use super::*;

fn digest(seed: u8) -> String {
    format!("sha256:{seed:064x}")
}

fn source(source_id: &str, version_id: &str, availability: SourceAvailability) -> SourceVersion {
    SourceVersion::new(SourceVersionFields {
        source_id: source_id.into(),
        version_id: version_id.into(),
        lifecycle: SourceLifecycle::Active,
        effective_date: Some("2026-01-01".into()),
        jurisdictions: vec!["US".into()],
        products: vec!["standard".into()],
        availability,
    })
    .expect("valid source version")
}

fn source_with_constraints(
    source_id: &str,
    version_id: &str,
    lifecycle: SourceLifecycle,
    effective_date: &str,
    jurisdictions: &[&str],
    products: &[&str],
) -> SourceVersion {
    SourceVersion::new(SourceVersionFields {
        source_id: source_id.into(),
        version_id: version_id.into(),
        lifecycle,
        effective_date: Some(effective_date.into()),
        jurisdictions: jurisdictions.iter().map(|value| (*value).into()).collect(),
        products: products.iter().map(|value| (*value).into()).collect(),
        availability: SourceAvailability::Authorized,
    })
    .expect("structurally valid source version")
}

fn scope() -> ComparisonScope {
    ComparisonScope::new(ComparisonScopeFields {
        scope_id: "scope-1".into(),
        left: source("policy", "v1", SourceAvailability::Authorized),
        right: source("policy", "v2", SourceAvailability::Authorized),
        comparison_question: Some("What changed?".into()),
    })
    .expect("valid two-sided scope")
}

fn dimension() -> ComparisonDimension {
    ComparisonDimension::new(ComparisonDimensionFields {
        dimension_id: "eligibility".into(),
        label: "Eligibility".into(),
        description: Some("Who qualifies".into()),
    })
    .expect("valid dimension")
}

fn value(source_id: &str, version_id: &str, wording: &str) -> DimensionValue {
    DimensionValue::new(DimensionValueFields {
        dimension_id: "eligibility".into(),
        source_id: source_id.into(),
        version_id: version_id.into(),
        normalized_value: Some(wording.into()),
        quotations: vec![QuotedEvidence {
            evidence_unit_id: format!("{version_id}-evidence"),
            quotation: wording.into(),
        }],
        interpretation: Some(format!("Interpretation of {wording}")),
        provenance: vec![EvidenceProvenance {
            evidence_unit_id: format!("{version_id}-evidence"),
            source_id: source_id.into(),
            version_id: version_id.into(),
            locator: "section 1".into(),
            content_hash: digest(if version_id == "v1" { 1 } else { 2 }),
        }],
    })
    .expect("value has quotation and provenance")
}

fn cell(alignment: DimensionAlignment) -> ComparisonCell {
    ComparisonCell {
        dimension: dimension(),
        left: Some(value("policy", "v1", "18 years")),
        right: Some(value("policy", "v2", "21 years")),
        alignment,
        interpretation: Some("The threshold changed.".into()),
    }
}

fn result_for_scope(scope: &ComparisonScope) -> ComparisonResult {
    ComparisonResult::new(
        ComparisonResultFields {
            result_id: "result-1".into(),
            scope_hash: content_hash_of(scope).expect("scope hashes"),
            cells: vec![cell(DimensionAlignment::Difference)],
            summary_interpretation: Some("Eligibility is narrower.".into()),
        },
        scope,
    )
    .expect("valid structured result")
}

fn result() -> ComparisonResult {
    let scope = scope();
    result_for_scope(&scope)
}

fn pack_for_scope(scope: &ComparisonScope, result: &ComparisonResult) -> ComparisonContextPack {
    ComparisonContextPack::new(
        ComparisonContextPackFields {
            pack_id: "pack-1".into(),
            scope_hash: content_hash_of(scope).expect("scope hashes"),
            comparison_result_hash: content_hash_of(result).expect("result hashes"),
            cells: result.cells.clone(),
            wire_context_pack: None,
        },
        scope,
        result,
    )
    .expect("valid reusable pack")
}

fn pack() -> ComparisonContextPack {
    let scope = scope();
    let result = result_for_scope(&scope);
    pack_for_scope(&scope, &result)
}

#[test]
fn scope_requires_distinct_versions_and_fails_closed_for_every_resolution_state() {
    let duplicate = ComparisonScope::new(ComparisonScopeFields {
        scope_id: "scope".into(),
        left: source("same", "v1", SourceAvailability::Authorized),
        right: source("same", "v1", SourceAvailability::Authorized),
        comparison_question: None,
    });
    assert!(matches!(duplicate, Err(ComparisonError::Validation { .. })));

    for (availability, expected) in [
        (SourceAvailability::AclDenied, "acl_denied"),
        (SourceAvailability::VersionGone, "version_gone"),
        (SourceAvailability::Unresolved, "scope_unresolved"),
    ] {
        let denied = ComparisonScope::new(ComparisonScopeFields {
            scope_id: format!("scope-{expected}"),
            left: source("left", "v1", availability),
            right: source("right", "v2", SourceAvailability::Authorized),
            comparison_question: None,
        })
        .expect("identity remains structurally valid until resolve");
        let error = denied.require_comparable().expect_err("must fail closed");
        assert_eq!(error.class_name(), expected);
    }
}

#[test]
fn scope_rejects_incompatible_lifecycle_and_declared_constraints() {
    scope()
        .require_comparable()
        .expect("both active authorized sides are comparable");
    let comparable = source_with_constraints(
        "right",
        "v2",
        SourceLifecycle::Active,
        "2026-01-01",
        &["US"],
        &["standard"],
    );
    for lifecycle in [
        SourceLifecycle::Superseded,
        SourceLifecycle::Retired,
        SourceLifecycle::Archived,
    ] {
        let incompatible = ComparisonScope::new(ComparisonScopeFields {
            scope_id: format!("scope-{}", lifecycle.as_str()),
            left: source_with_constraints(
                "left",
                "v1",
                lifecycle,
                "2026-01-01",
                &["US"],
                &["standard"],
            ),
            right: comparable.clone(),
            comparison_question: None,
        })
        .expect("identity remains structurally valid until comparison");
        assert!(matches!(
            incompatible.require_comparable(),
            Err(ComparisonError::ScopeUnresolved { .. })
        ));
    }

    for (scope_id, effective_date, jurisdictions, products) in [
        ("date", "2027-01-01", vec!["US"], vec!["standard"]),
        ("jurisdiction", "2026-01-01", vec!["CA"], vec!["standard"]),
        ("product", "2026-01-01", vec!["US"], vec!["premium"]),
    ] {
        let incompatible = ComparisonScope::new(ComparisonScopeFields {
            scope_id: format!("scope-{scope_id}"),
            left: source_with_constraints(
                "left",
                "v1",
                SourceLifecycle::Active,
                effective_date,
                &jurisdictions,
                &products,
            ),
            right: comparable.clone(),
            comparison_question: None,
        })
        .expect("identity remains structurally valid until comparison");
        assert!(matches!(
            incompatible.require_comparable(),
            Err(ComparisonError::ScopeUnresolved { .. })
        ));
    }
}

#[test]
fn dimension_value_rejects_unbound_quotation_and_cell_enforces_alignment_evidence() {
    let unbound = DimensionValue::new(DimensionValueFields {
        dimension_id: "eligibility".into(),
        source_id: "policy".into(),
        version_id: "v1".into(),
        normalized_value: None,
        quotations: vec![QuotedEvidence {
            evidence_unit_id: "unbound".into(),
            quotation: "Quoted source text".into(),
        }],
        interpretation: None,
        provenance: vec![EvidenceProvenance {
            evidence_unit_id: "bound".into(),
            source_id: "policy".into(),
            version_id: "v1".into(),
            locator: "section 1".into(),
            content_hash: "sha256:bound".into(),
        }],
    });
    assert!(matches!(
        unbound,
        Err(ComparisonError::MissingEvidence { .. })
    ));

    let missing_both = ComparisonCell {
        dimension: dimension(),
        left: None,
        right: None,
        alignment: DimensionAlignment::Agreement,
        interpretation: None,
    };
    assert!(matches!(
        missing_both.validate_for_scope(&scope()),
        Err(ComparisonError::MissingEvidence { .. })
    ));

    let false_missing = cell(DimensionAlignment::Missing);
    assert!(matches!(
        false_missing.validate_for_scope(&scope()),
        Err(ComparisonError::Validation { .. })
    ));
}

#[test]
fn every_alignment_class_has_valid_or_explicitly_rejected_shape() {
    for alignment in [
        DimensionAlignment::Agreement,
        DimensionAlignment::Difference,
        DimensionAlignment::Conflict,
        DimensionAlignment::Incomparable,
    ] {
        cell(alignment)
            .validate_for_scope(&scope())
            .expect("two evidenced values are legal");
    }
    let missing = ComparisonCell {
        dimension: dimension(),
        left: Some(value("policy", "v1", "18 years")),
        right: None,
        alignment: DimensionAlignment::Missing,
        interpretation: None,
    };
    missing
        .validate_for_scope(&scope())
        .expect("missing preserves the available side evidence");
}

#[test]
fn each_budget_cap_is_fail_closed_and_never_silently_clamped() {
    let budget = ComparisonBudget::new(ComparisonBudgetFields {
        max_dimensions: 1,
        max_sources: 2,
        max_candidates: 1,
        max_tokens: 1,
        max_cost_units: 1,
        max_wall_time_ms: 1,
    })
    .expect("positive bounded budget");
    let excesses = [
        (
            ComparisonBudgetUsage {
                dimensions: 2,
                ..Default::default()
            },
            ComparisonBudgetDimension::Dimensions,
        ),
        (
            ComparisonBudgetUsage {
                sources: 3,
                ..Default::default()
            },
            ComparisonBudgetDimension::Sources,
        ),
        (
            ComparisonBudgetUsage {
                candidates: 2,
                ..Default::default()
            },
            ComparisonBudgetDimension::Candidates,
        ),
        (
            ComparisonBudgetUsage {
                tokens: 2,
                ..Default::default()
            },
            ComparisonBudgetDimension::Tokens,
        ),
        (
            ComparisonBudgetUsage {
                cost_units: 2,
                ..Default::default()
            },
            ComparisonBudgetDimension::CostUnits,
        ),
        (
            ComparisonBudgetUsage {
                wall_time_ms: 2,
                ..Default::default()
            },
            ComparisonBudgetDimension::WallTimeMs,
        ),
    ];
    for (usage, dimension) in excesses {
        let error = usage
            .check_against(&budget)
            .expect_err("hard cap must reject");
        assert!(matches!(
            error,
            ComparisonError::BudgetExhausted { exhaustion, .. } if exhaustion.dimension == dimension
        ));
    }
}

#[test]
fn budget_constructor_rejects_multi_source_execution_contract() {
    let invalid = ComparisonBudget::new(ComparisonBudgetFields {
        max_dimensions: 1,
        max_sources: 3,
        max_candidates: 1,
        max_tokens: 1,
        max_cost_units: 1,
        max_wall_time_ms: 1,
    });
    assert!(matches!(invalid, Err(ComparisonError::Validation { .. })));
}

#[test]
fn state_machine_allows_only_ordered_pipeline_and_completion_needs_rendered_artifacts() {
    let mut run = start_run(
        "run-1".into(),
        &scope(),
        ComparisonBudget::skeleton_default(),
    )
    .expect("authorized scope starts");
    assert!(matches!(
        advance_stage(&mut run, ComparisonStage::Extracting),
        Err(ComparisonError::IllegalTransition { .. })
    ));
    for stage in [
        ComparisonStage::Resolving,
        ComparisonStage::Extracting,
        ComparisonStage::Aligning,
        ComparisonStage::Rendering,
    ] {
        assert!(matches!(
            advance_stage(&mut run, stage),
            Ok(StageAdvance::Advanced(_))
        ));
    }
    let mut rendered_result = result();
    rendered_result.scope_hash = run.scope_hash.clone();
    let mut rendered_pack = pack();
    rendered_pack.scope_hash = run.scope_hash.clone();
    rendered_pack.comparison_result_hash =
        content_hash_of(&rendered_result).expect("result hashes");
    let complete = try_complete(&mut run, &rendered_result, &rendered_pack)
        .expect("rendered artifacts complete");
    assert!(matches!(complete, ComparisonOutcome::Complete(_)));
    assert_eq!(run.status, ComparisonRunStatus::Complete);
    assert!(matches!(
        advance_stage(&mut run, ComparisonStage::Complete),
        Ok(StageAdvance::AlreadyTerminal(ComparisonStage::Complete))
    ));
}

#[test]
fn record_stage_rejects_wrong_active_stage_and_preserves_usage_on_exhaustion() {
    let mut run = start_run(
        "run-1".into(),
        &scope(),
        ComparisonBudget::skeleton_default(),
    )
    .expect("starts");
    let wrong = record_stage(
        &mut run,
        ComparisonStageRecord {
            stage: ComparisonStage::Resolving,
            artifact_hash: Some(digest(5)),
            input_fingerprint: None,
            output_fingerprint: None,
            usage_delta: ComparisonBudgetUsage::default(),
            cost: ComparisonCost::default(),
            ok: true,
            detail: None,
        },
    );
    assert!(matches!(
        wrong,
        Err(ComparisonError::IllegalTransition { .. })
    ));
    let before = run.usage.clone();
    let exhausted = record_stage(
        &mut run,
        ComparisonStageRecord {
            stage: ComparisonStage::Decomposing,
            artifact_hash: Some(digest(6)),
            input_fingerprint: None,
            output_fingerprint: None,
            usage_delta: ComparisonBudgetUsage {
                tokens: 50_001,
                ..Default::default()
            },
            cost: ComparisonCost::default(),
            ok: true,
            detail: None,
        },
    );
    assert!(matches!(
        exhausted,
        Err(ComparisonError::BudgetExhausted { .. })
    ));
    assert_eq!(run.usage, before, "failed accounting must not mutate usage");
}

#[test]
fn record_stage_accounts_cost_and_rejects_cost_only_exhaustion() {
    let budget = ComparisonBudget::new(ComparisonBudgetFields {
        max_dimensions: 1,
        max_sources: 2,
        max_candidates: 1,
        max_tokens: u64::MAX - 1,
        max_cost_units: 1,
        max_wall_time_ms: 1,
    })
    .expect("bounded budget");
    let mut run = start_run("run-cost".into(), &scope(), budget).expect("starts");
    let before = run.usage.clone();
    let cost_only = record_stage(
        &mut run,
        ComparisonStageRecord {
            stage: ComparisonStage::Decomposing,
            artifact_hash: Some(digest(7)),
            input_fingerprint: None,
            output_fingerprint: None,
            usage_delta: ComparisonBudgetUsage::default(),
            cost: ComparisonCost {
                cost_units: 2,
                ..Default::default()
            },
            ok: true,
            detail: None,
        },
    );
    assert!(matches!(
        cost_only,
        Err(ComparisonError::BudgetExhausted { exhaustion, .. })
            if exhaustion.dimension == ComparisonBudgetDimension::CostUnits
    ));
    assert_eq!(
        run.usage, before,
        "failed cost accounting must not mutate usage"
    );
}

#[test]
fn fail_closed_marks_acl_budget_and_missing_evidence_incomplete_but_disabled_distinct() {
    for error in [
        ComparisonError::AclDenied {
            source_id: "s".into(),
            version_id: "v".into(),
        },
        ComparisonError::budget_exhausted(
            ComparisonBudgetExhaustion::new(ComparisonBudgetDimension::Tokens, 1, 2),
            "token cap",
        ),
        ComparisonError::missing_evidence("citation absent"),
    ] {
        let mut run = start_run("run".into(), &scope(), ComparisonBudget::skeleton_default())
            .expect("starts");
        assert!(matches!(
            fail_closed(&mut run, error),
            Ok(ComparisonOutcome::Incomplete(_))
        ));
        assert_eq!(run.status, ComparisonRunStatus::Incomplete);
    }
    let mut run =
        start_run("run".into(), &scope(), ComparisonBudget::skeleton_default()).expect("starts");
    assert!(matches!(
        fail_closed(&mut run, ComparisonError::disabled("not configured")),
        Ok(ComparisonOutcome::Disabled(_))
    ));
    assert_eq!(run.status, ComparisonRunStatus::Disabled);
}

#[test]
fn completion_rejects_cross_scope_or_mismatched_result_artifacts() {
    let run_scope = scope();
    let mut other_scope = scope();
    other_scope.scope_id = "scope-2".into();
    let other_scope_hash = content_hash_of(&other_scope).expect("other scope hashes");

    let mut cross_scope_result = result();
    cross_scope_result.scope_hash = other_scope_hash.clone();
    let mut cross_scope_pack = pack();
    cross_scope_pack.scope_hash = other_scope_hash;
    cross_scope_pack.comparison_result_hash =
        content_hash_of(&cross_scope_result).expect("result hashes");

    let mut cross_scope_run = start_run(
        "cross-scope".into(),
        &run_scope,
        ComparisonBudget::skeleton_default(),
    )
    .expect("starts");
    for stage in [
        ComparisonStage::Resolving,
        ComparisonStage::Extracting,
        ComparisonStage::Aligning,
        ComparisonStage::Rendering,
    ] {
        advance_stage(&mut cross_scope_run, stage).expect("advances");
    }
    assert!(matches!(
        try_complete(&mut cross_scope_run, &cross_scope_result, &cross_scope_pack),
        Err(ComparisonError::Validation { .. })
    ));

    let mut result_for_run = result();
    result_for_run.scope_hash = cross_scope_run.scope_hash.clone();
    let mut mismatched_pack = pack();
    mismatched_pack.scope_hash = cross_scope_run.scope_hash.clone();
    mismatched_pack.comparison_result_hash = digest(7);
    assert!(matches!(
        try_complete(&mut cross_scope_run, &result_for_run, &mismatched_pack),
        Err(ComparisonError::Validation { .. })
    ));

    assert!(matches!(
        ComparisonResult::new(
            ComparisonResultFields {
                result_id: "malformed-digest".into(),
                scope_hash: "not-a-digest".into(),
                cells: vec![cell(DimensionAlignment::Difference)],
                summary_interpretation: None,
            },
            &scope(),
        ),
        Err(ComparisonError::Validation { .. })
    ));
}

#[test]
fn artifact_constructors_bind_hashes_to_the_supplied_scope_and_result() {
    let scope = scope();
    let valid_result = result_for_scope(&scope);
    let mut other_scope = scope.clone();
    other_scope.scope_id = "other-scope".into();
    let unrelated_scope_hash = content_hash_of(&other_scope).expect("other scope hashes");

    assert!(matches!(
        ComparisonResult::new(
            ComparisonResultFields {
                result_id: "wrong-scope".into(),
                scope_hash: unrelated_scope_hash,
                cells: vec![cell(DimensionAlignment::Difference)],
                summary_interpretation: None,
            },
            &scope,
        ),
        Err(ComparisonError::Validation { .. })
    ));

    assert!(matches!(
        ComparisonContextPack::new(
            ComparisonContextPackFields {
                pack_id: "wrong-result".into(),
                scope_hash: content_hash_of(&scope).expect("scope hashes"),
                comparison_result_hash: digest(4),
                cells: valid_result.cells.clone(),
                wire_context_pack: None,
            },
            &scope,
            &valid_result,
        ),
        Err(ComparisonError::Validation { .. })
    ));
}

#[test]
fn terminal_runs_cannot_be_completed_or_transitioned_again() {
    let mut disabled = start_run(
        "disabled".into(),
        &scope(),
        ComparisonBudget::skeleton_default(),
    )
    .expect("starts");
    for stage in [
        ComparisonStage::Resolving,
        ComparisonStage::Extracting,
        ComparisonStage::Aligning,
        ComparisonStage::Rendering,
    ] {
        advance_stage(&mut disabled, stage).expect("advances");
    }
    disabled
        .mark_disabled("disabled for test")
        .expect("disables");
    assert!(matches!(
        disabled.complete(digest(8), digest(9)),
        Err(ComparisonError::IllegalTransition { .. })
    ));

    let mut complete = start_run(
        "complete".into(),
        &scope(),
        ComparisonBudget::skeleton_default(),
    )
    .expect("starts");
    for stage in [
        ComparisonStage::Resolving,
        ComparisonStage::Extracting,
        ComparisonStage::Aligning,
        ComparisonStage::Rendering,
    ] {
        advance_stage(&mut complete, stage).expect("advances");
    }
    complete
        .complete(digest(10), digest(11))
        .expect("completes");
    assert!(matches!(
        complete.complete(digest(12), digest(13)),
        Err(ComparisonError::IllegalTransition { .. })
    ));
    assert!(matches!(
        fail_closed(
            &mut complete,
            ComparisonError::missing_evidence("late failure")
        ),
        Err(ComparisonError::IllegalTransition { .. })
    ));

    let mut incomplete = start_run(
        "incomplete".into(),
        &scope(),
        ComparisonBudget::skeleton_default(),
    )
    .expect("starts");
    incomplete
        .mark_incomplete("incomplete for test")
        .expect("marks incomplete");

    for run in [&mut disabled, &mut complete, &mut incomplete] {
        let before = run.clone();
        assert!(matches!(
            run.record_stage(ComparisonStageRecord {
                stage: run.current_stage,
                artifact_hash: None,
                input_fingerprint: None,
                output_fingerprint: None,
                usage_delta: ComparisonBudgetUsage::default(),
                cost: ComparisonCost::default(),
                ok: true,
                detail: None,
            }),
            Err(ComparisonError::IllegalTransition { .. })
        ));
        assert!(matches!(
            run.add_warning(ComparisonWarning {
                severity: ComparisonWarningSeverity::Info,
                code: "late".into(),
                detail: "terminal mutation".into(),
            }),
            Err(ComparisonError::IllegalTransition { .. })
        ));
        assert_eq!(*run, before, "terminal public mutators must not mutate");
    }
}

#[test]
fn run_json_round_trip_and_unknown_schema_fail_closed() {
    let run = start_run(
        "run-1".into(),
        &scope(),
        ComparisonBudget::skeleton_default(),
    )
    .expect("starts");
    let bytes = encode_workflow_run_json(&run).expect("encodes");
    assert_eq!(decode_workflow_run_json(&bytes).expect("decodes"), run);
    let mut unknown = run;
    unknown.schema_version = 99;
    let bytes = serde_json::to_vec(&unknown).expect("test JSON serialization");
    assert!(matches!(
        decode_workflow_run_json(&bytes),
        Err(ComparisonError::Validation { .. })
    ));
}

#[test]
fn context_pack_and_result_preserve_unresolved_alignment_visibility() {
    let scope = scope();
    let conflict = ComparisonResult::new(
        ComparisonResultFields {
            result_id: "conflict-result".into(),
            scope_hash: content_hash_of(&scope).expect("scope hashes"),
            cells: vec![cell(DimensionAlignment::Conflict)],
            summary_interpretation: None,
        },
        &scope,
    )
    .expect("conflict remains a structured supported output");
    assert!(conflict.has_unresolved_cells());
    let pack = pack();
    assert_eq!(
        pack.cells[0]
            .left
            .as_ref()
            .expect("test fixture left")
            .quotations[0]
            .quotation,
        "18 years"
    );
    assert_eq!(
        pack.cells[0]
            .left
            .as_ref()
            .expect("test fixture left")
            .interpretation
            .as_deref(),
        Some("Interpretation of 18 years")
    );
}

#[test]
fn error_taxonomy_variants_are_named_and_budget_exhaustion_validates() {
    let exhaustion = ComparisonBudgetExhaustion::new(ComparisonBudgetDimension::Tokens, 1, 2);
    exhaustion.validate().expect("used exceeds cap");
    for error in [
        ComparisonError::validation("bad"),
        ComparisonError::scope_unresolved("pending"),
        ComparisonError::VersionGone {
            source_id: "s".into(),
            version_id: "v".into(),
        },
        ComparisonError::AclDenied {
            source_id: "s".into(),
            version_id: "v".into(),
        },
        ComparisonError::budget_exhausted(exhaustion, "over"),
        ComparisonError::missing_evidence("none"),
        ComparisonError::IllegalTransition {
            detail: "bad edge".into(),
        },
        ComparisonError::disabled("off"),
    ] {
        assert!(!error.class_name().is_empty());
    }
}
