use super::*;

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
            content_hash: format!("sha256:{version_id}"),
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

fn result() -> ComparisonResult {
    ComparisonResult::new(
        ComparisonResultFields {
            result_id: "result-1".into(),
            scope_hash: "sha256:scope".into(),
            cells: vec![cell(DimensionAlignment::Difference)],
            summary_interpretation: Some("Eligibility is narrower.".into()),
        },
        &scope(),
    )
    .expect("valid structured result")
}

fn pack() -> ComparisonContextPack {
    ComparisonContextPack::new(
        ComparisonContextPackFields {
            pack_id: "pack-1".into(),
            scope_hash: "sha256:scope".into(),
            comparison_result_hash: "sha256:result".into(),
            cells: result().cells,
            wire_context_pack: None,
        },
        &scope(),
    )
    .expect("valid reusable pack")
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
fn all_lifecycle_availability_alignment_and_stage_variants_are_reachable() {
    for lifecycle in [
        SourceLifecycle::Active,
        SourceLifecycle::Superseded,
        SourceLifecycle::Retired,
        SourceLifecycle::Archived,
    ] {
        assert!(!lifecycle.as_str().is_empty());
    }
    for availability in [
        SourceAvailability::Authorized,
        SourceAvailability::AclDenied,
        SourceAvailability::VersionGone,
        SourceAvailability::Unresolved,
    ] {
        assert!(!availability.as_str().is_empty());
    }
    for alignment in [
        DimensionAlignment::Agreement,
        DimensionAlignment::Difference,
        DimensionAlignment::Conflict,
        DimensionAlignment::Missing,
        DimensionAlignment::Incomparable,
    ] {
        assert!(!alignment.as_str().is_empty());
    }
    for stage in ComparisonStage::all() {
        assert!(!stage.as_str().is_empty());
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
    let complete = try_complete(&mut run, &result(), &pack()).expect("rendered artifacts complete");
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
            artifact_hash: Some("sha256:resolve".into()),
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
            artifact_hash: Some("sha256:decompose".into()),
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
    let conflict = ComparisonResult::new(
        ComparisonResultFields {
            result_id: "conflict-result".into(),
            scope_hash: "sha256:scope".into(),
            cells: vec![cell(DimensionAlignment::Conflict)],
            summary_interpretation: None,
        },
        &scope(),
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
