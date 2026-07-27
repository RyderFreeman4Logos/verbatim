use super::*;

fn digest(seed: u8) -> String {
    format!("sha256:{seed:064x}")
}

fn member(source_id: &str) -> AuditScopeMember {
    AuditScopeMember::new(AuditScopeMemberFields {
        collection_id: "policies".into(),
        source_id: source_id.into(),
        snapshot_id: format!("{source_id}-snapshot"),
        freshness: ScopeFreshness::CheckedFresh,
        index_coverage: ScopeIndexCoverage::Complete,
    })
    .expect("valid declared member")
}

fn scope() -> DeclaredAuditScope {
    DeclaredAuditScope::new(DeclaredAuditScopeFields {
        scope_id: "policy-audit".into(),
        members: vec![member("policy-a"), member("policy-b")],
    })
    .expect("valid declared scope")
}

fn exhaustive_coverage(scope: &DeclaredAuditScope) -> CoverageManifest {
    CoverageManifest::new(CoverageManifestFields {
        scope_hash: content_hash_of(scope).expect("scope hash"),
        entries: scope
            .members
            .iter()
            .map(|member| CoverageEntry {
                member_key: member.key(),
                status: ScopeCoverageStatus::Searched,
                detail: None,
            })
            .collect(),
    })
    .expect("complete manifest")
}

fn primary_enumeration(scope: &DeclaredAuditScope) -> CandidateEnumeration {
    CandidateEnumeration::new(CandidateEnumerationFields {
        enumeration_id: "lexical-all".into(),
        scope_hash: content_hash_of(scope).expect("scope hash"),
        method: EnumerationMethod::Lexical,
        query_fingerprint: digest(3),
        candidate_ids: vec!["a-v1".into(), "b-v1".into()],
        deterministic: true,
    })
    .expect("valid lexical enumeration")
}

#[test]
fn scope_requires_declared_collection_source_snapshot_and_fresh_complete_index() {
    let duplicate = DeclaredAuditScope::new(DeclaredAuditScopeFields {
        scope_id: "dup".into(),
        members: vec![member("policy-a"), member("policy-a")],
    });
    assert!(matches!(
        duplicate,
        Err(ExhaustiveAuditError::Validation { .. })
    ));

    for (freshness, index_coverage) in [
        (ScopeFreshness::Stale, ScopeIndexCoverage::Complete),
        (ScopeFreshness::CheckedFresh, ScopeIndexCoverage::Partial),
        (
            ScopeFreshness::CheckedFresh,
            ScopeIndexCoverage::Unsupported,
        ),
    ] {
        let member = AuditScopeMember::new(AuditScopeMemberFields {
            collection_id: "policies".into(),
            source_id: "policy".into(),
            snapshot_id: "snapshot".into(),
            freshness,
            index_coverage,
        })
        .expect("structural scope member remains recordable");
        let scope = DeclaredAuditScope::new(DeclaredAuditScopeFields {
            scope_id: format!("{}-{}", freshness.as_str(), index_coverage.as_str()),
            members: vec![member],
        })
        .expect("declared scope remains recordable");
        assert!(scope.require_deterministic_coverage().is_err());
    }
}

#[test]
fn only_all_none_and_every_require_a_complete_deterministic_manifest() {
    let scope = scope();
    let coverage = exhaustive_coverage(&scope);
    let enumeration = primary_enumeration(&scope);
    for target in [
        CompletenessTarget::All,
        CompletenessTarget::Only,
        CompletenessTarget::None,
        CompletenessTarget::Every,
    ] {
        assert_eq!(
            establish_completeness(target, &scope, &coverage, &[enumeration.clone()])
                .expect("valid evidence"),
            CompletenessStatus::ExhaustiveOverDeclaredScope
        );
    }

    let incomplete = CoverageManifest::new(CoverageManifestFields {
        scope_hash: content_hash_of(&scope).expect("scope hash"),
        entries: vec![
            CoverageEntry {
                member_key: scope.members[0].key(),
                status: ScopeCoverageStatus::Searched,
                detail: None,
            },
            CoverageEntry {
                member_key: scope.members[1].key(),
                status: ScopeCoverageStatus::Unsearched,
                detail: Some("collection scan unavailable".into()),
            },
        ],
    })
    .expect("incomplete manifest is recordable");
    for target in [CompletenessTarget::Only, CompletenessTarget::None] {
        assert_eq!(
            establish_completeness(target, &scope, &incomplete, &[enumeration.clone()])
                .expect("valid incomplete evidence"),
            CompletenessStatus::Incomplete
        );
    }
}

#[test]
fn ann_and_top_k_are_supplementary_and_never_establish_exhaustiveness() {
    let scope = scope();
    let coverage = exhaustive_coverage(&scope);
    for method in [
        EnumerationMethod::DenseAnn,
        EnumerationMethod::Graph,
        EnumerationMethod::TopK,
    ] {
        let approximate = CandidateEnumeration::new(CandidateEnumerationFields {
            enumeration_id: format!("{}-pass", method.as_str()),
            scope_hash: content_hash_of(&scope).expect("scope hash"),
            method,
            query_fingerprint: digest(8),
            candidate_ids: vec!["a-v1".into()],
            deterministic: false,
        })
        .expect("supplementary enumeration is recordable");
        assert!(!approximate.is_primary());
        assert_eq!(
            establish_completeness(CompletenessTarget::All, &scope, &coverage, &[approximate])
                .expect("valid evidence"),
            CompletenessStatus::UnableToEstablish
        );
    }
}

#[test]
fn coverage_manifest_projects_unsearched_blocked_stale_and_unsupported_fail_closed() {
    let scope = scope();
    for status in [
        ScopeCoverageStatus::Unsearched,
        ScopeCoverageStatus::Blocked,
        ScopeCoverageStatus::Stale,
        ScopeCoverageStatus::Unsupported,
    ] {
        let manifest = CoverageManifest::new(CoverageManifestFields {
            scope_hash: content_hash_of(&scope).expect("scope hash"),
            entries: scope
                .members
                .iter()
                .enumerate()
                .map(|(index, member)| CoverageEntry {
                    member_key: member.key(),
                    status: if index == 0 {
                        status
                    } else {
                        ScopeCoverageStatus::Searched
                    },
                    detail: Some("reason".into()),
                })
                .collect(),
        })
        .expect("coverage record is valid");
        let expected = if status == ScopeCoverageStatus::Blocked {
            CompletenessStatus::Blocked
        } else {
            CompletenessStatus::Incomplete
        };
        assert_eq!(
            establish_completeness(
                CompletenessTarget::Every,
                &scope,
                &manifest,
                &[primary_enumeration(&scope)],
            )
            .expect("valid evidence"),
            expected
        );
    }
}

#[test]
fn dedup_retains_versioned_occurrence_counts_and_locators() {
    let deduped = DeduplicatedCandidate::new(DeduplicatedCandidateFields {
        canonical_id: "policy-42".into(),
        near_duplicate_key: Some("policy 42".into()),
        occurrences: vec![
            CandidateOccurrence {
                candidate_id: "policy-42-v1".into(),
                version_id: "v1".into(),
                locator: "policies/a.md#42".into(),
            },
            CandidateOccurrence {
                candidate_id: "policy-42-v2".into(),
                version_id: "v2".into(),
                locator: "policies/b.md#42".into(),
            },
        ],
    })
    .expect("deduped candidate retains all occurrences");
    assert_eq!(deduped.occurrence_count(), 2);
    assert!(DeduplicatedCandidate::new(DeduplicatedCandidateFields {
        canonical_id: "bad".into(),
        near_duplicate_key: None,
        occurrences: vec![],
    })
    .is_err());
}

#[test]
fn run_only_allows_legal_stages_and_binds_a_fail_closed_status() {
    let scope = scope();
    let mut run = AuditWorkflowRun::new(AuditWorkflowRunFields {
        run_id: "audit-run".into(),
        scope_hash: content_hash_of(&scope).expect("scope hash"),
        target: CompletenessTarget::All,
        budget: ExhaustiveAuditBudget::skeleton_default(),
    })
    .expect("new run");
    assert!(advance_stage(&mut run, AuditStage::Reconciling).is_err());
    for stage in [
        AuditStage::Enumerating,
        AuditStage::Covering,
        AuditStage::Reconciling,
        AuditStage::Reporting,
    ] {
        advance_stage(&mut run, stage).expect("legal stage transition");
    }
    let outcome = report(
        &mut run,
        &scope,
        &exhaustive_coverage(&scope),
        &[primary_enumeration(&scope)],
    )
    .expect("report evidence");
    assert_eq!(
        outcome.status(),
        CompletenessStatus::ExhaustiveOverDeclaredScope
    );
    assert_eq!(run.status, CompletenessStatus::ExhaustiveOverDeclaredScope);
    assert_eq!(run.current_stage, AuditStage::Complete);
}

#[test]
fn budgets_fail_closed_before_usage_mutation() {
    let budget = ExhaustiveAuditBudget::new(ExhaustiveAuditBudgetFields {
        max_scope_members: 2,
        max_enumerations: 1,
        max_candidates: 2,
        max_cost_units: 3,
        max_wall_time_ms: 4,
    })
    .expect("valid budget");
    let usage = ExhaustiveAuditUsage::default();
    let err = usage
        .checked_add(
            &ExhaustiveAuditUsage {
                candidates: 3,
                ..ExhaustiveAuditUsage::default()
            },
            &budget,
        )
        .expect_err("candidate cap must fail");
    assert!(matches!(err, ExhaustiveAuditError::BudgetExhausted { .. }));
}

#[test]
fn budgets_fail_closed_on_arithmetic_overflow() {
    let budget = ExhaustiveAuditBudget::new(ExhaustiveAuditBudgetFields {
        max_scope_members: u64::MAX,
        max_enumerations: u64::MAX,
        max_candidates: u64::MAX,
        max_cost_units: u64::MAX,
        max_wall_time_ms: u64::MAX,
    })
    .expect("maximum budget is valid");
    let err = ExhaustiveAuditUsage {
        candidates: u64::MAX,
        ..ExhaustiveAuditUsage::default()
    }
    .checked_add(
        &ExhaustiveAuditUsage {
            candidates: 1,
            ..ExhaustiveAuditUsage::default()
        },
        &budget,
    )
    .expect_err("overflow must exhaust the candidate budget");
    assert!(matches!(
        err,
        ExhaustiveAuditError::BudgetExhausted {
            exhaustion: ExhaustiveAuditBudgetExhaustion {
                dimension: ExhaustiveAuditBudgetDimension::Candidates,
                limit: u64::MAX,
                used: u64::MAX,
            }
        }
    ));
}

#[test]
fn forged_exhaustive_json_requires_canonical_evidence() {
    let digest = format!("sha256:{}", "0".repeat(64));
    let bytes = serde_json::to_vec(&serde_json::json!({
        "schema_version": EXHAUSTIVE_AUDIT_WORKFLOW_SCHEMA_VERSION,
        "run_id": "forged-run",
        "scope_hash": digest,
        "target": "all",
        "current_stage": "complete",
        "status": "exhaustive_over_declared_scope",
        "budget": {
            "max_scope_members": 1,
            "max_enumerations": 1,
            "max_candidates": 1,
            "max_cost_units": 1,
            "max_wall_time_ms": 1,
        },
        "usage": {
            "scope_members": 0,
            "enumerations": 0,
            "candidates": 0,
            "cost_units": 0,
            "wall_time_ms": 0,
        },
        "stage_records": [],
        "enumeration_hashes": [],
        "coverage_manifest_hash": null,
        "reconciliation_hash": null,
        "report_hash": format!("sha256:{}", "1".repeat(64)),
        "query_fingerprints": [],
        "warnings": [],
    }))
    .expect("encode forged exhaustive fixture");

    assert!(decode_audit_workflow_run_json(&bytes).is_err());
}

#[test]
fn workflow_run_json_roundtrip_rejects_unknown_schema_and_unbound_exhaustive_status() {
    let scope = scope();
    let mut run = AuditWorkflowRun::new(AuditWorkflowRunFields {
        run_id: "serial-run".into(),
        scope_hash: content_hash_of(&scope).expect("scope hash"),
        target: CompletenessTarget::None,
        budget: ExhaustiveAuditBudget::skeleton_default(),
    })
    .expect("new run");
    for stage in [
        AuditStage::Enumerating,
        AuditStage::Covering,
        AuditStage::Reconciling,
        AuditStage::Reporting,
    ] {
        advance_stage(&mut run, stage).expect("legal stage transition");
    }
    report(
        &mut run,
        &scope,
        &exhaustive_coverage(&scope),
        &[primary_enumeration(&scope)],
    )
    .expect("bound exhaustive report");
    let bytes = encode_audit_workflow_run_json(&run).expect("encodes run");
    assert_eq!(
        decode_audit_workflow_run_json(&bytes).expect("decodes run"),
        run
    );

    let mut forged = run.clone();
    forged.coverage_manifest_hash = None;
    let bytes = serde_json::to_vec(&forged).expect("encode forged exhaustive fixture");
    assert!(decode_audit_workflow_run_json(&bytes).is_err());

    let mut unknown = run.clone();
    unknown.schema_version = 99;
    let bytes = serde_json::to_vec(&unknown).expect("encode invalid schema fixture");
    assert!(decode_audit_workflow_run_json(&bytes).is_err());

    let mut unbound = run;
    unbound.report_hash = None;
    assert!(unbound.validate().is_err());
}
