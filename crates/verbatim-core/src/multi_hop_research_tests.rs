//! Contract tests for bounded multi-hop research workflow (WORKFLOW-006 / #357).

use std::collections::BTreeSet;

use super::*;

fn sample_budget() -> ResearchBudget {
    ResearchBudget::new(ResearchBudgetFields {
        max_rounds: 3,
        max_subqueries: 16,
        max_candidates: 200,
        max_tokens: 50_000,
        max_endpoint_calls: 32,
        max_cost_units: 100,
        max_wall_time_ms: 60_000,
    })
    .unwrap()
}

fn sample_question() -> ResearchQuestion {
    ResearchQuestion::new(ResearchQuestionFields {
        question_id: "rq-1".into(),
        text: "Who acquired the vendor of opaque-id-X?".into(),
        required_facts: vec!["vendor_of_x".into(), "acquirer".into()],
        required_relations: vec!["acquired".into()],
        query_plan_hash: Some("qp-hash-1".into()),
    })
    .unwrap()
}

fn sample_plan(question: &ResearchQuestion) -> DecompositionPlan {
    let q_hash = content_hash_of(question).unwrap();
    DecompositionPlan::new(DecompositionPlanFields {
        plan_id: "dp-1".into(),
        research_question_id: question.question_id.clone(),
        research_question_hash: q_hash,
        subquestions: vec![
            SubQuestion::new(SubQuestionFields {
                id: "sq-a".into(),
                text: "What vendor produces opaque-id-X?".into(),
                depends_on: vec![],
                retrievers: vec![RetrieverKind::Lexical, RetrieverKind::Dense],
                targets_facts: vec!["vendor_of_x".into()],
                targets_relations: vec![],
            })
            .unwrap(),
            SubQuestion::new(SubQuestionFields {
                id: "sq-b".into(),
                text: "Who acquired that vendor?".into(),
                depends_on: vec!["sq-a".into()],
                retrievers: vec![RetrieverKind::GraphLocal, RetrieverKind::Exact],
                targets_facts: vec!["acquirer".into()],
                targets_relations: vec!["acquired".into()],
            })
            .unwrap(),
        ],
    })
    .unwrap()
}

fn complete_coverage(round_index: u32) -> CoverageReport {
    CoverageReport::new(CoverageReportFields {
        report_id: format!("cov-{round_index}"),
        round_index,
        facts: vec![
            FactCoverage {
                fact: "vendor_of_x".into(),
                status: CoverageStatus::Covered,
                evidence_unit_ids: vec!["eu-1".into()],
                subquestion_ids: vec!["sq-a".into()],
                note: None,
            },
            FactCoverage {
                fact: "acquirer".into(),
                status: CoverageStatus::Covered,
                evidence_unit_ids: vec!["eu-2".into()],
                subquestion_ids: vec!["sq-b".into()],
                note: None,
            },
        ],
        relations: vec![RelationCoverage {
            relation: "acquired".into(),
            status: CoverageStatus::Covered,
            evidence_unit_ids: vec!["eu-2".into()],
            subquestion_ids: vec!["sq-b".into()],
            note: None,
        }],
        conflicts: vec![],
    })
    .unwrap()
}

fn incomplete_coverage(round_index: u32) -> CoverageReport {
    CoverageReport::new(CoverageReportFields {
        report_id: format!("cov-inc-{round_index}"),
        round_index,
        facts: vec![
            FactCoverage {
                fact: "vendor_of_x".into(),
                status: CoverageStatus::Covered,
                evidence_unit_ids: vec!["eu-1".into()],
                subquestion_ids: vec!["sq-a".into()],
                note: None,
            },
            FactCoverage {
                fact: "acquirer".into(),
                status: CoverageStatus::Missing,
                evidence_unit_ids: vec![],
                subquestion_ids: vec![],
                note: Some("bridge missing".into()),
            },
        ],
        relations: vec![RelationCoverage {
            relation: "acquired".into(),
            status: CoverageStatus::Partial,
            evidence_unit_ids: vec!["eu-1".into()],
            subquestion_ids: vec!["sq-a".into()],
            note: None,
        }],
        conflicts: vec![],
    })
    .unwrap()
}

fn sample_merged(question: &ResearchQuestion, plan: &DecompositionPlan) -> MergedContextPack {
    MergedContextPack::new(MergedContextPackFields {
        pack_id: "mcp-1".into(),
        research_question_hash: content_hash_of(question).unwrap(),
        decomposition_plan_hash: content_hash_of(plan).unwrap(),
        units: vec![
            AttributedEvidenceUnit {
                evidence_unit_id: "eu-1".into(),
                subquestion_ids: vec![SubQuestionId::new("sq-a").unwrap()],
                is_direct: true,
                evidence_pack_hash: Some("ep-1".into()),
            },
            AttributedEvidenceUnit {
                evidence_unit_id: "eu-2".into(),
                subquestion_ids: vec![SubQuestionId::new("sq-b").unwrap()],
                is_direct: true,
                evidence_pack_hash: Some("ep-2".into()),
            },
        ],
        context_pack_hash: Some("cp-hash-1".into()),
        generation: Some("gen-1".into()),
        profile_ref: Some("profile:default".into()),
    })
    .unwrap()
}

#[test]
fn research_round_all_and_terminal() {
    assert_eq!(ResearchRound::all().len(), 6);
    assert!(ResearchRound::Complete.is_terminal());
    assert!(ResearchRound::Incomplete.is_terminal());
    assert!(!ResearchRound::Retrieving.is_terminal());
    for r in ResearchRound::all() {
        assert_eq!(r.as_str().is_empty(), false);
    }
}

#[test]
fn advance_round_legal_and_illegal() {
    let adv = advance_round(
        ResearchRound::Decomposing,
        ResearchTransition::StartRetrieval,
    )
    .unwrap();
    assert_eq!(adv, RoundAdvance::Advanced(ResearchRound::Retrieving));

    let err = advance_round(ResearchRound::Decomposing, ResearchTransition::Complete).unwrap_err();
    assert_eq!(err.class_name(), "illegal_transition");

    let term = advance_round(ResearchRound::Complete, ResearchTransition::StartRetrieval).unwrap();
    assert_eq!(term, RoundAdvance::AlreadyTerminal(ResearchRound::Complete));
}

#[test]
fn decomposition_rejects_cycle_and_unknown_dep() {
    let question = sample_question();
    let q_hash = content_hash_of(&question).unwrap();
    let err = DecompositionPlan::new(DecompositionPlanFields {
        plan_id: "dp-cycle".into(),
        research_question_id: question.question_id.clone(),
        research_question_hash: q_hash.clone(),
        subquestions: vec![
            SubQuestion::new(SubQuestionFields {
                id: "a".into(),
                text: "A".into(),
                depends_on: vec!["b".into()],
                retrievers: vec![RetrieverKind::Lexical],
                targets_facts: vec![],
                targets_relations: vec![],
            })
            .unwrap(),
            SubQuestion::new(SubQuestionFields {
                id: "b".into(),
                text: "B".into(),
                depends_on: vec!["a".into()],
                retrievers: vec![RetrieverKind::Dense],
                targets_facts: vec![],
                targets_relations: vec![],
            })
            .unwrap(),
        ],
    })
    .unwrap_err();
    assert!(err.to_string().contains("cycle"));

    let err = DecompositionPlan::new(DecompositionPlanFields {
        plan_id: "dp-unk".into(),
        research_question_id: question.question_id.clone(),
        research_question_hash: q_hash,
        subquestions: vec![SubQuestion::new(SubQuestionFields {
            id: "a".into(),
            text: "A".into(),
            depends_on: vec!["missing".into()],
            retrievers: vec![RetrieverKind::Lexical],
            targets_facts: vec![],
            targets_relations: vec![],
        })
        .unwrap()],
    })
    .unwrap_err();
    assert!(err.to_string().contains("unknown"));
}

#[test]
fn ready_subquestions_respects_dependencies() {
    let question = sample_question();
    let plan = sample_plan(&question);
    let ready = plan.ready_subquestions(&BTreeSet::new());
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id.as_str(), "sq-a");

    let mut done = BTreeSet::new();
    done.insert(SubQuestionId::new("sq-a").unwrap());
    let ready = plan.ready_subquestions(&done);
    assert_eq!(ready.len(), 1);
    assert_eq!(ready[0].id.as_str(), "sq-b");
}

#[test]
fn budget_exhaustion_is_typed() {
    let budget = sample_budget();
    let mut usage = ResearchBudgetUsage::default();
    usage.rounds = 3;
    // rounds == max is ok; starting another corrective must fail.
    usage.check_against(&budget).unwrap();
    assert!(matches!(
        usage.may_start_corrective_round(&budget),
        Err(ResearchError::BudgetExhausted { .. })
    ));
    assert_eq!(
        usage
            .may_start_corrective_round(&budget)
            .unwrap_err()
            .class_name(),
        "budget_exhausted"
    );

    let mut over = ResearchBudgetUsage::default();
    over.candidates = 201;
    let err = over.check_against(&budget).unwrap_err();
    match err {
        ResearchError::BudgetExhausted {
            exhaustion: BudgetExhaustion { dimension, .. },
            ..
        } => assert_eq!(dimension, BudgetDimension::Candidates),
        other => panic!("expected budget exhausted, got {other}"),
    }
}

#[test]
fn injection_guard_rejects_evidence_as_instruction() {
    let origin = EvidenceOrigin::new(EvidenceOriginFields {
        origin: EvidenceOriginKind::EvidenceText,
        content_ref: "eu-inject".into(),
        note: Some("doc://evil".into()),
    })
    .unwrap();
    let err = guard_instruction_origin(&origin).unwrap_err();
    assert_eq!(err.class_name(), "injection_rejected");
    assert!(!origin.origin.may_alter_workflow_control());

    let control = EvidenceOrigin::new(EvidenceOriginFields {
        origin: EvidenceOriginKind::WorkflowInstruction,
        content_ref: "ctrl".into(),
        note: None,
    })
    .unwrap();
    assert!(control.origin.may_alter_workflow_control());
    guard_instruction_origin(&control).unwrap();
}

#[test]
fn subquery_result_rejects_control_origins() {
    let err = SubqueryResult {
        request_id: "req-1".into(),
        subquestion_id: SubQuestionId::new("sq-a").unwrap(),
        provenance: RetrieverProvenance {
            retriever: RetrieverKind::Lexical,
            endpoint_fingerprint: "ep-fp".into(),
            candidates_considered: 10,
            evidence_unit_ids: vec!["eu-1".into()],
            evidence_pack_hash: None,
        },
        evidence_origins: vec![EvidenceOrigin::new(EvidenceOriginFields {
            origin: EvidenceOriginKind::WorkflowInstruction,
            content_ref: "eu-1".into(),
            note: None,
        })
        .unwrap()],
        ok: true,
        detail: None,
    }
    .validate()
    .unwrap_err();
    assert_eq!(err.class_name(), "injection_rejected");
}

#[test]
fn parallel_batch_and_result_roundtrip() {
    let batch = ParallelRetrievalBatch::new(ParallelRetrievalBatchFields {
        batch_id: "batch-1".into(),
        round_index: 1,
        requests: vec![SubqueryRequest::new(SubqueryRequestFields {
            request_id: "req-1".into(),
            subquestion_id: "sq-a".into(),
            retriever: RetrieverKind::Lexical,
            query_text: "vendor of opaque-id-X".into(),
            query_plan_hash: None,
            max_candidates: 20,
        })
        .unwrap()],
    })
    .unwrap();
    let bytes = serde_json::to_vec(&batch).unwrap();
    let back: ParallelRetrievalBatch = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back, batch);

    let result = ParallelRetrievalBatchResult {
        batch_id: "batch-1".into(),
        round_index: 1,
        results: vec![SubqueryResult {
            request_id: "req-1".into(),
            subquestion_id: SubQuestionId::new("sq-a").unwrap(),
            provenance: RetrieverProvenance {
                retriever: RetrieverKind::Lexical,
                endpoint_fingerprint: "lex-1".into(),
                candidates_considered: 12,
                evidence_unit_ids: vec!["eu-1".into()],
                evidence_pack_hash: Some("ep-hash".into()),
            },
            evidence_origins: vec![EvidenceOrigin::new(EvidenceOriginFields {
                origin: EvidenceOriginKind::EvidenceText,
                content_ref: "eu-1".into(),
                note: Some("doc://a".into()),
            })
            .unwrap()],
            ok: true,
            detail: None,
        }],
    };
    result.validate().unwrap();
    assert_eq!(result.total_candidates(), 12);
}

#[test]
fn coverage_report_complete_and_unresolved() {
    let complete = complete_coverage(1);
    assert!(complete.is_complete);
    assert!(complete.unresolved_requirements.is_empty());
    assert!(!complete.needs_corrective_round());

    let incomplete = incomplete_coverage(1);
    assert!(!incomplete.is_complete);
    assert!(incomplete.needs_corrective_round());
    assert!(incomplete
        .unresolved_requirements
        .iter()
        .any(|u| u == "acquirer"));
    assert!(incomplete
        .unresolved_requirements
        .iter()
        .any(|u| u == "acquired"));
}

#[test]
fn coverage_conflict_blocks_complete() {
    let report = CoverageReport::new(CoverageReportFields {
        report_id: "cov-conflict".into(),
        round_index: 1,
        facts: vec![FactCoverage {
            fact: "vendor_of_x".into(),
            status: CoverageStatus::Covered,
            evidence_unit_ids: vec!["eu-1".into(), "eu-2".into()],
            subquestion_ids: vec!["sq-a".into()],
            note: None,
        }],
        relations: vec![],
        conflicts: vec![CoverageConflict {
            conflict_id: "c1".into(),
            summary: "sources disagree on vendor".into(),
            evidence_unit_ids: vec!["eu-1".into(), "eu-2".into()],
        }],
    })
    .unwrap();
    assert!(!report.is_complete);
    assert!(report
        .unresolved_requirements
        .iter()
        .any(|u| u == "conflict:c1"));
}

#[test]
fn merge_attributed_units_dedups_and_unions() {
    let a = AttributedEvidenceUnit {
        evidence_unit_id: "eu-1".into(),
        subquestion_ids: vec![SubQuestionId::new("sq-a").unwrap()],
        is_direct: true,
        evidence_pack_hash: Some("ep-a".into()),
    };
    let b = AttributedEvidenceUnit {
        evidence_unit_id: "eu-1".into(),
        subquestion_ids: vec![SubQuestionId::new("sq-b").unwrap()],
        is_direct: false,
        evidence_pack_hash: None,
    };
    let c = AttributedEvidenceUnit {
        evidence_unit_id: "eu-2".into(),
        subquestion_ids: vec![SubQuestionId::new("sq-b").unwrap()],
        is_direct: true,
        evidence_pack_hash: Some("ep-b".into()),
    };
    let merged = merge_attributed_units(&[vec![a], vec![b, c]]).unwrap();
    assert_eq!(merged.len(), 2);
    assert_eq!(merged[0].evidence_unit_id, "eu-1");
    assert_eq!(merged[0].subquestion_ids.len(), 2);
    assert!(merged[0].is_direct);
    assert_eq!(merged[1].evidence_unit_id, "eu-2");
}

#[test]
fn workflow_run_complete_path_and_json_roundtrip() {
    let question = sample_question();
    let plan = sample_plan(&question);
    let budget = sample_budget();
    let q_hash = content_hash_of(&question).unwrap();
    let mut run = WorkflowRun::new(WorkflowRunFields {
        run_id: "run-1".into(),
        research_question_hash: q_hash,
        budget,
        profile_ref: Some("profile:default".into()),
        generation: Some("gen-1".into()),
    })
    .unwrap();

    record_decomposition(&mut run, &plan).unwrap();
    assert_eq!(run.current_round, ResearchRound::Retrieving);
    assert!(run.decomposition_plan_hash.is_some());

    // Simulate retrieval usage.
    run.record_round(ResearchRoundRecord {
        round: ResearchRound::EvaluatingCoverage,
        round_index: 1,
        artifact_hash: Some(content_hash_of(&complete_coverage(1)).unwrap()),
        usage_delta: ResearchBudgetUsage {
            subqueries: 2,
            candidates: 20,
            endpoint_calls: 2,
            ..Default::default()
        },
        ok: true,
        detail: None,
    })
    .unwrap();

    let pack = sample_merged(&question, &plan);
    let outcome = try_complete(&mut run, &pack).unwrap();
    assert!(matches!(outcome, ResearchOutcome::Complete { .. }));
    assert_eq!(outcome.final_status(), ResearchFinalStatus::Complete);
    assert!(outcome.run().merged_context_pack_hash.is_some());

    let bytes = encode_workflow_run_json(outcome.run()).unwrap();
    let decoded = decode_workflow_run_json(&bytes).unwrap();
    assert_eq!(&decoded, outcome.run());
}

#[test]
fn workflow_run_incomplete_on_budget_and_coverage() {
    let question = sample_question();
    let plan = sample_plan(&question);
    let tight = ResearchBudget::new(ResearchBudgetFields {
        max_rounds: 1,
        max_subqueries: 4,
        max_candidates: 50,
        max_tokens: 1000,
        max_endpoint_calls: 4,
        max_cost_units: 10,
        max_wall_time_ms: 5_000,
    })
    .unwrap();
    let mut run = WorkflowRun::new(WorkflowRunFields {
        run_id: "run-2".into(),
        research_question_hash: content_hash_of(&question).unwrap(),
        budget: tight,
        profile_ref: None,
        generation: None,
    })
    .unwrap();
    record_decomposition(&mut run, &plan).unwrap();
    // usage.rounds == 1 after decomposition helper; corrective not allowed.
    let report = incomplete_coverage(1);
    record_coverage(&mut run, &report).unwrap();
    let decision = decide_after_coverage(&report, &run).unwrap();
    assert_eq!(decision, CoverageDecision::IncompleteBudget);

    let err = ResearchError::budget_exhausted(
        BudgetExhaustion {
            dimension: BudgetDimension::Rounds,
            limit: 1,
            used: 2,
        },
        "no corrective rounds remaining",
    );
    let outcome = fail_closed(&mut run, &err).unwrap();
    assert!(matches!(outcome, ResearchOutcome::Incomplete { .. }));
    assert_eq!(outcome.run().current_round, ResearchRound::Incomplete);
    assert!(outcome.run().incomplete_reason.is_some());
}

#[test]
fn fail_closed_disabled() {
    let question = sample_question();
    let mut run = WorkflowRun::new(WorkflowRunFields {
        run_id: "run-dis".into(),
        research_question_hash: content_hash_of(&question).unwrap(),
        budget: sample_budget(),
        profile_ref: None,
        generation: None,
    })
    .unwrap();
    let outcome = fail_closed(&mut run, &ResearchError::disabled("no multi-hop profile")).unwrap();
    assert!(matches!(outcome, ResearchOutcome::Disabled { .. }));
    assert_eq!(outcome.final_status(), ResearchFinalStatus::Disabled);
}

#[test]
fn complete_requires_legal_round() {
    let question = sample_question();
    let plan = sample_plan(&question);
    let mut run = WorkflowRun::new(WorkflowRunFields {
        run_id: "run-3".into(),
        research_question_hash: content_hash_of(&question).unwrap(),
        budget: sample_budget(),
        profile_ref: None,
        generation: None,
    })
    .unwrap();
    let pack = sample_merged(&question, &plan);
    let err = run.complete(&pack).unwrap_err();
    assert_eq!(err.class_name(), "illegal_transition");
}

#[test]
fn public_wire_types_json_roundtrip() {
    let question = sample_question();
    let plan = sample_plan(&question);
    let report = complete_coverage(1);
    let pack = sample_merged(&question, &plan);
    let budget = sample_budget();

    for (name, bytes) in [
        ("question", serde_json::to_vec(&question).unwrap()),
        ("plan", serde_json::to_vec(&plan).unwrap()),
        ("report", serde_json::to_vec(&report).unwrap()),
        ("pack", serde_json::to_vec(&pack).unwrap()),
        ("budget", serde_json::to_vec(&budget).unwrap()),
    ] {
        assert!(!bytes.is_empty(), "{name} empty");
    }

    let q2: ResearchQuestion =
        serde_json::from_slice(&serde_json::to_vec(&question).unwrap()).unwrap();
    assert_eq!(q2, question);
    let p2: DecompositionPlan =
        serde_json::from_slice(&serde_json::to_vec(&plan).unwrap()).unwrap();
    assert_eq!(p2, plan);
    let r2: CoverageReport = serde_json::from_slice(&serde_json::to_vec(&report).unwrap()).unwrap();
    assert_eq!(r2, report);
    let m2: MergedContextPack =
        serde_json::from_slice(&serde_json::to_vec(&pack).unwrap()).unwrap();
    assert_eq!(m2, pack);

    // Unknown schema version fails closed.
    let mut run = WorkflowRun::new(WorkflowRunFields {
        run_id: "run-schema".into(),
        research_question_hash: content_hash_of(&question).unwrap(),
        budget: sample_budget(),
        profile_ref: None,
        generation: None,
    })
    .unwrap();
    run.schema_version = 99;
    assert!(run.validate().is_err());
    let bad = serde_json::to_vec(&run).unwrap();
    assert!(decode_workflow_run_json(&bad).is_err());
}

#[test]
fn research_error_requires_abstention_except_disabled() {
    let budget_err = ResearchError::budget_exhausted(
        BudgetExhaustion::new(BudgetDimension::Tokens, 1, 2),
        "tokens",
    );
    assert!(budget_err.requires_incomplete());
    assert!(!ResearchError::disabled("x").requires_incomplete());
    assert_eq!(RetrieverKind::GraphGlobal.requires_edge_evidence(), true);
    assert_eq!(RetrieverKind::Lexical.requires_edge_evidence(), false);
}

#[test]
fn enum_as_str_exhaustive_smoke() {
    for d in [
        BudgetDimension::Rounds,
        BudgetDimension::Subqueries,
        BudgetDimension::Candidates,
        BudgetDimension::Tokens,
        BudgetDimension::EndpointCalls,
        BudgetDimension::CostUnits,
        BudgetDimension::WallTimeMs,
    ] {
        assert!(!d.as_str().is_empty());
    }
    for s in [
        CoverageStatus::Covered,
        CoverageStatus::Partial,
        CoverageStatus::Missing,
        CoverageStatus::Conflict,
    ] {
        assert!(!s.as_str().is_empty());
    }
    for k in [
        EvidenceOriginKind::WorkflowInstruction,
        EvidenceOriginKind::PolicyConfig,
        EvidenceOriginKind::EvidenceText,
        EvidenceOriginKind::DocumentBody,
        EvidenceOriginKind::ModelIntermediate,
    ] {
        assert!(!k.as_str().is_empty());
    }
    for s in [
        ResearchFinalStatus::Complete,
        ResearchFinalStatus::Incomplete,
        ResearchFinalStatus::Disabled,
        ResearchFinalStatus::InProgress,
    ] {
        assert!(!s.as_str().is_empty());
    }
}
