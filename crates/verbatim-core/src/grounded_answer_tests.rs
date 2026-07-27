//! Contract tests for bounded grounded-answer workflow (WORKFLOW-005 / #356).

use super::*;
use crate::wire_schemas::{
    encode_wire_document, ContextPackEnvelope, ContextPackFields, EvidencePackEnvelope,
    EvidencePackFields, QueryPlanEnvelope, QueryPlanFields, WorkflowPhase,
};

fn sample_query_plan() -> QueryPlanEnvelope {
    QueryPlanEnvelope::new(QueryPlanFields {
        artifact_id: "qp-ga-1".into(),
        query_text: "what is the retention policy?".into(),
        steps: vec!["lexical".into(), "vector".into()],
        generation: Some("gen-1".into()),
        profile_ref: Some("profile:default".into()),
    })
    .unwrap()
}

fn sample_evidence_pack(plan_hash: &str) -> EvidencePackEnvelope {
    EvidencePackEnvelope::new(EvidencePackFields {
        artifact_id: "ep-ga-1".into(),
        evidence_unit_ids: vec!["eu-a".into(), "eu-b".into()],
        query_plan_hash: plan_hash.into(),
        generation: Some("gen-1".into()),
        profile_ref: Some("profile:default".into()),
    })
    .unwrap()
}

fn sample_context_pack(evidence_hash: &str) -> ContextPackEnvelope {
    ContextPackEnvelope::new(ContextPackFields {
        artifact_id: "cp-ga-1".into(),
        evidence_pack_hash: evidence_hash.into(),
        selected_unit_ids: vec!["eu-a".into()],
        model_fingerprint: Some("model-fp-1".into()),
        generation: Some("gen-1".into()),
        profile_ref: Some("profile:default".into()),
    })
    .unwrap()
}

fn sample_policy(model_enabled: bool) -> WorkflowPolicyContext {
    WorkflowPolicyContext::new(WorkflowPolicyContextFields {
        principal: "user:alice".into(),
        profile_ref: "profile:default".into(),
        policy_version: "policy-v1".into(),
        model_enabled,
        remaining_revisions: 1,
        remaining_cost_units: Some(100),
    })
    .unwrap()
}

fn sample_plan(context_hash: &str) -> AnswerPlan {
    AnswerPlan::new(AnswerPlanFields {
        plan_id: "ap-1".into(),
        context_pack_hash: context_hash.into(),
        instruction: "Answer only from evidence.".into(),
        allowed_evidence_unit_ids: vec!["eu-a".into()],
        max_claims: 3,
        model_fingerprint: Some("model-fp-1".into()),
    })
    .unwrap()
}

fn sample_draft(plan: &AnswerPlan) -> AnswerDraft {
    let plan_hash = content_hash_of(plan).unwrap();
    AnswerDraft {
        draft_id: "draft-1".into(),
        answer_plan_hash: plan_hash,
        context_pack_hash: plan.context_pack_hash.clone(),
        model_fingerprint: "model-fp-1".into(),
        body: "Retention is 30 days.".into(),
        claims: vec![DraftClaim::new(DraftClaimFields {
            claim_id: "c1".into(),
            text: "Retention is 30 days.".into(),
            cited_evidence_unit_ids: vec!["eu-a".into()],
            quotation: Some("retain for 30 days".into()),
        })
        .unwrap()],
    }
}

fn publishable_verdict() -> ClaimVerdict {
    ClaimVerdict {
        claim_id: ClaimId::new("c1").unwrap(),
        support: ClaimSupportClass::Supported,
        quotation_checks: vec![QuotationCheck::new("eu-a", QuotationCheckStatus::Match).unwrap()],
        notes: vec![],
    }
}

fn sample_grounded_answer(plan_hash: &str, context_hash: &str) -> GroundedAnswer {
    let citations = render_citations(
        &CitationRenderRequest::new(CitationRenderRequestFields {
            style: CitationStyle::BracketedSequential,
            body: "Retention is 30 days.".into(),
            claim_bindings: vec![ClaimCitationBinding::new("c1", vec!["eu-a".into()]).unwrap()],
        })
        .unwrap(),
    )
    .unwrap();

    GroundedAnswer::new(GroundedAnswerFields {
        answer_id: "ga-1".into(),
        context_pack_hash: context_hash.into(),
        query_plan_hash: plan_hash.into(),
        model_fingerprint: "model-fp-1".into(),
        claims: vec![GroundedClaim::new(GroundedClaimFields {
            claim_id: "c1".into(),
            text: "Retention is 30 days.".into(),
            evidence_unit_ids: vec!["eu-a".into()],
        })
        .unwrap()],
        citations,
    })
    .unwrap()
}

// ---------------------------------------------------------------------------
// Stages / transitions
// ---------------------------------------------------------------------------

#[test]
fn workflow_stage_all_is_exhaustive() {
    let all = WorkflowStage::all();
    assert_eq!(all.len(), 8);
    assert!(all.contains(&WorkflowStage::Planned));
    assert!(all.contains(&WorkflowStage::Published));
    assert!(all.contains(&WorkflowStage::Abstained));
    for stage in all {
        assert_eq!(stage.as_str().is_empty(), false);
        let _ = stage.to_string();
    }
}

#[test]
fn happy_path_stage_machine() {
    let mut stage = WorkflowStage::Planned;
    for transition in [
        WorkflowTransition::StartRetrieve,
        WorkflowTransition::StartAssemble,
        WorkflowTransition::StartGenerate,
        WorkflowTransition::StartVerify,
        WorkflowTransition::StartRender,
        WorkflowTransition::Publish,
    ] {
        let advance = advance_stage(stage, transition).unwrap();
        stage = advance.to;
    }
    assert_eq!(stage, WorkflowStage::Published);
    assert!(stage.is_terminal());
    assert!(advance_stage(stage, WorkflowTransition::StartRetrieve).is_err());
}

#[test]
fn illegal_transition_fails_closed() {
    let err = advance_stage(WorkflowStage::Planned, WorkflowTransition::Publish).unwrap_err();
    assert_eq!(err.class_name(), "illegal_transition");
    assert!(err.requires_abstention());
}

#[test]
fn revise_once_from_verifying() {
    let advance = advance_stage(WorkflowStage::Verifying, WorkflowTransition::ReviseOnce).unwrap();
    assert_eq!(advance.to, WorkflowStage::Generating);
}

// ---------------------------------------------------------------------------
// Claims / verification
// ---------------------------------------------------------------------------

#[test]
fn claim_support_publishable_only_supported() {
    assert!(ClaimSupportClass::Supported.is_publishable());
    assert!(!ClaimSupportClass::Partial.is_publishable());
    assert!(!ClaimSupportClass::Conflict.is_publishable());
    assert!(!ClaimSupportClass::Unsupported.is_publishable());
    assert!(!ClaimSupportClass::NonFactual.is_publishable());
}

#[test]
fn verification_report_all_publishable_roundtrip() {
    let report = ClaimVerificationReport::new(ClaimVerificationReportFields {
        context_pack_hash: "cp-hash-1".into(),
        draft_hash: "draft-hash-1".into(),
        verdicts: vec![publishable_verdict()],
        revise_allowed: false,
    })
    .unwrap();
    assert!(report.all_publishable);
    assert_eq!(
        decide_after_verification(&report).unwrap(),
        WorkflowTransition::StartRender
    );

    let bytes = encode_wire_document(&report).unwrap();
    let back: ClaimVerificationReport = serde_json::from_slice(&bytes).unwrap();
    back.validate().unwrap();
    assert_eq!(back, report);
}

#[test]
fn verification_partial_forces_abstain_or_revise() {
    let mut verdict = publishable_verdict();
    verdict.support = ClaimSupportClass::Partial;
    let report = ClaimVerificationReport::new(ClaimVerificationReportFields {
        context_pack_hash: "cp-hash-1".into(),
        draft_hash: "draft-hash-1".into(),
        verdicts: vec![verdict],
        revise_allowed: false,
    })
    .unwrap();
    assert!(!report.all_publishable);
    assert_eq!(
        decide_after_verification(&report).unwrap(),
        WorkflowTransition::Abstain
    );

    let mut verdict = publishable_verdict();
    verdict.support = ClaimSupportClass::Conflict;
    let report = ClaimVerificationReport::new(ClaimVerificationReportFields {
        context_pack_hash: "cp-hash-1".into(),
        draft_hash: "draft-hash-1".into(),
        verdicts: vec![verdict],
        revise_allowed: true,
    })
    .unwrap();
    assert_eq!(
        decide_after_verification(&report).unwrap(),
        WorkflowTransition::ReviseOnce
    );
}

#[test]
fn empty_verdicts_rejected() {
    let err = ClaimVerificationReport::new(ClaimVerificationReportFields {
        context_pack_hash: "cp-hash-1".into(),
        draft_hash: "draft-hash-1".into(),
        verdicts: vec![],
        revise_allowed: false,
    })
    .unwrap_err();
    assert_eq!(err.class_name(), "validation");
}

#[test]
fn unknown_evidence_not_publishable() {
    let verdict = ClaimVerdict {
        claim_id: ClaimId::new("c1").unwrap(),
        support: ClaimSupportClass::Supported,
        quotation_checks: vec![QuotationCheck::new(
            "eu-missing",
            QuotationCheckStatus::UnknownEvidence,
        )
        .unwrap()],
        notes: vec![],
    };
    assert!(!verdict.is_publishable());
}

#[test]
fn all_publishable_ignores_non_factual_when_factual_supported() {
    let report = ClaimVerificationReport::new(ClaimVerificationReportFields {
        context_pack_hash: "cp-hash-1".into(),
        draft_hash: "draft-hash-1".into(),
        verdicts: vec![
            publishable_verdict(),
            ClaimVerdict {
                claim_id: ClaimId::new("meta").unwrap(),
                support: ClaimSupportClass::NonFactual,
                quotation_checks: vec![],
                notes: vec!["meta remark".into()],
            },
        ],
        revise_allowed: false,
    })
    .unwrap();
    assert!(report.all_publishable);
    assert_eq!(
        decide_after_verification(&report).unwrap(),
        WorkflowTransition::StartRender
    );
}

#[test]
fn all_publishable_false_when_only_non_factual() {
    let report = ClaimVerificationReport::new(ClaimVerificationReportFields {
        context_pack_hash: "cp-hash-1".into(),
        draft_hash: "draft-hash-1".into(),
        verdicts: vec![ClaimVerdict {
            claim_id: ClaimId::new("meta").unwrap(),
            support: ClaimSupportClass::NonFactual,
            quotation_checks: vec![],
            notes: vec![],
        }],
        revise_allowed: false,
    })
    .unwrap();
    assert!(!report.all_publishable);
}

// ---------------------------------------------------------------------------
// Citations
// ---------------------------------------------------------------------------

#[test]
fn citation_render_deterministic() {
    let req = CitationRenderRequest::new(CitationRenderRequestFields {
        style: CitationStyle::BracketedSequential,
        body: "Alpha. Beta.".into(),
        claim_bindings: vec![
            ClaimCitationBinding::new("c1", vec!["eu-a".into()]).unwrap(),
            ClaimCitationBinding::new("c2", vec!["eu-b".into()]).unwrap(),
            ClaimCitationBinding::new("c3", vec!["eu-a".into()]).unwrap(),
        ],
    })
    .unwrap();
    let a = render_citations(&req).unwrap();
    let b = render_citations(&req).unwrap();
    assert_eq!(a, b);
    assert_eq!(a.citations[0].label, "[E1]");
    assert_eq!(a.citations[1].label, "[E2]");
    // Same evidence reuses sequential label.
    assert_eq!(a.citations[2].label, "[E1]");
    assert!(a.rendered_text.contains("Citations:"));
}

#[test]
fn citation_render_rejects_empty_bindings() {
    let err = CitationRenderRequest::new(CitationRenderRequestFields {
        style: CitationStyle::EvidenceUnitId,
        body: "x".into(),
        claim_bindings: vec![],
    })
    .unwrap_err();
    assert_eq!(err.class_name(), "validation");
}

// ---------------------------------------------------------------------------
// Answer artifacts
// ---------------------------------------------------------------------------

#[test]
fn answer_plan_and_draft_validate() {
    let plan = sample_query_plan();
    let ep = sample_evidence_pack(plan.header.identity.content_hash.as_str());
    let cp = sample_context_pack(ep.header.identity.content_hash.as_str());
    let answer_plan = sample_plan(cp.header.identity.content_hash.as_str());
    answer_plan.validate().unwrap();
    let draft = sample_draft(&answer_plan);
    draft.validate().unwrap();
    assert!(draft.cites_only_allowed(&answer_plan));
}

#[test]
fn draft_rejects_empty_claims() {
    let plan = sample_plan("context-hash-1");
    let mut draft = sample_draft(&plan);
    draft.claims.clear();
    assert!(draft.validate().is_err());
}

#[test]
fn grounded_answer_requires_claim_citation_bijection() {
    let plan = sample_query_plan();
    let plan_hash = plan.header.identity.content_hash.as_str();
    let answer = sample_grounded_answer(plan_hash, "context-hash-1");
    answer.validate().unwrap();

    let mut bad = answer.clone();
    bad.claims.push(
        GroundedClaim::new(GroundedClaimFields {
            claim_id: "orphan".into(),
            text: "no citation".into(),
            evidence_unit_ids: vec!["eu-a".into()],
        })
        .unwrap(),
    );
    assert!(bad.validate().is_err());
}

#[test]
fn grounded_answer_rejects_citation_evidence_not_bound_to_claim() {
    let plan = sample_query_plan();
    let plan_hash = plan.header.identity.content_hash.as_str();
    let mut answer = sample_grounded_answer(plan_hash, "context-hash-1");
    // Claim still asserts eu-a, but citation is rewritten to point at eu-b.
    answer.citations.citations[0].evidence_unit_id = "eu-b".into();
    let err = answer.validate().unwrap_err();
    assert_eq!(err.class_name(), "validation");
    assert!(
        err.to_string().contains("evidence_unit_id"),
        "expected evidence binding failure, got {err}"
    );
}

#[test]
fn grounded_answer_json_roundtrip() {
    let plan = sample_query_plan();
    let answer = sample_grounded_answer(plan.header.identity.content_hash.as_str(), "ctx-hash");
    let bytes = encode_wire_document(&answer).unwrap();
    let back: GroundedAnswer = serde_json::from_slice(&bytes).unwrap();
    back.validate().unwrap();
    assert_eq!(back, answer);
}

// ---------------------------------------------------------------------------
// WorkflowRun envelope
// ---------------------------------------------------------------------------

#[test]
fn workflow_run_happy_path_publish() {
    let plan = sample_query_plan();
    let plan_hash = plan.header.identity.content_hash.as_str().to_string();
    let mut run = WorkflowRun::new(WorkflowRunFields {
        run_id: "run-1".into(),
        query_plan_hash: plan_hash.clone(),
        profile_ref: Some("profile:default".into()),
        generation: Some("gen-1".into()),
    })
    .unwrap();
    assert_eq!(run.final_status, WorkflowFinalStatus::InProgress);

    run.record_stage(WorkflowStageRecord {
        stage: WorkflowStage::Retrieving,
        artifact_hash: Some("ep-hash".into()),
        model_fingerprint: None,
        cost: Some(WorkflowCost {
            model_calls: 0,
            input_tokens: 0,
            output_tokens: 0,
            cost_units: 1,
        }),
        ok: true,
        detail: None,
    })
    .unwrap();
    run.record_stage(WorkflowStageRecord {
        stage: WorkflowStage::Assembling,
        artifact_hash: Some("cp-hash".into()),
        model_fingerprint: None,
        cost: None,
        ok: true,
        detail: None,
    })
    .unwrap();
    run.record_stage(WorkflowStageRecord {
        stage: WorkflowStage::Generating,
        artifact_hash: Some("ap-hash".into()),
        model_fingerprint: Some("model-fp-1".into()),
        cost: Some(WorkflowCost {
            model_calls: 1,
            input_tokens: 10,
            output_tokens: 5,
            cost_units: 2,
        }),
        ok: true,
        detail: None,
    })
    .unwrap();
    run.record_stage(WorkflowStageRecord {
        stage: WorkflowStage::Generating,
        artifact_hash: Some("draft-hash".into()),
        model_fingerprint: Some("model-fp-1".into()),
        cost: None,
        ok: true,
        detail: None,
    })
    .unwrap();
    run.record_stage(WorkflowStageRecord {
        stage: WorkflowStage::Verifying,
        artifact_hash: None,
        model_fingerprint: None,
        cost: None,
        ok: true,
        detail: None,
    })
    .unwrap();
    run.record_stage(WorkflowStageRecord {
        stage: WorkflowStage::Rendering,
        artifact_hash: None,
        model_fingerprint: None,
        cost: None,
        ok: true,
        detail: None,
    })
    .unwrap();

    let answer = sample_grounded_answer(&plan_hash, "cp-hash");
    let outcome = try_publish(run, answer).unwrap();
    assert!(outcome.is_published());
    assert_eq!(outcome.run().final_status, WorkflowFinalStatus::Published);
    assert!(outcome.run().grounded_answer_hash.is_some());
    assert!(outcome.run().total_cost.model_calls >= 1);
}

#[test]
fn workflow_run_abstain_fail_closed() {
    let plan = sample_query_plan();
    let run = WorkflowRun::new(WorkflowRunFields {
        run_id: "run-abs".into(),
        query_plan_hash: plan.header.identity.content_hash.as_str().into(),
        profile_ref: None,
        generation: None,
    })
    .unwrap();
    let outcome = fail_closed(run, WorkflowError::model_failure("timeout")).unwrap();
    assert!(outcome.is_abstained());
    assert_eq!(outcome.run().final_status, WorkflowFinalStatus::Abstained);
    assert!(outcome.run().abstention_reason.is_some());
    // Model failure must never produce a published answer.
    assert!(!outcome.is_published());
}

#[test]
fn workflow_run_disabled_when_no_model() {
    let policy = sample_policy(false);
    assert!(policy.require_model_enabled().is_err());

    let plan = sample_query_plan();
    let run = WorkflowRun::new(WorkflowRunFields {
        run_id: "run-dis".into(),
        query_plan_hash: plan.header.identity.content_hash.as_str().into(),
        profile_ref: Some("profile:default".into()),
        generation: None,
    })
    .unwrap();
    let outcome = fail_closed(run, WorkflowError::disabled("no text-model endpoint")).unwrap();
    match outcome {
        WorkflowOutcome::Disabled { .. } => {}
        other => panic!("expected disabled, got {other:?}"),
    }
}

#[test]
fn workflow_run_json_roundtrip() {
    let plan = sample_query_plan();
    let mut run = WorkflowRun::new(WorkflowRunFields {
        run_id: "run-rt".into(),
        query_plan_hash: plan.header.identity.content_hash.as_str().into(),
        profile_ref: Some("profile:default".into()),
        generation: Some("gen-1".into()),
    })
    .unwrap();
    run.record_stage(WorkflowStageRecord {
        stage: WorkflowStage::Retrieving,
        artifact_hash: Some("ep-hash".into()),
        model_fingerprint: None,
        cost: None,
        ok: true,
        detail: None,
    })
    .unwrap();
    let bytes = encode_workflow_run_json(&run).unwrap();
    let back = decode_workflow_run_json(&bytes).unwrap();
    assert_eq!(back, run);
    // Byte-stable encode.
    assert_eq!(encode_workflow_run_json(&back).unwrap(), bytes);
}

#[test]
fn unknown_schema_version_fails_closed() {
    let plan = sample_query_plan();
    let mut run = WorkflowRun::new(WorkflowRunFields {
        run_id: "run-sv".into(),
        query_plan_hash: plan.header.identity.content_hash.as_str().into(),
        profile_ref: None,
        generation: None,
    })
    .unwrap();
    run.schema_version = 99;
    assert!(run.validate().is_err());
}

#[test]
fn publish_without_rendering_fails() {
    let plan = sample_query_plan();
    let plan_hash = plan.header.identity.content_hash.as_str().to_string();
    let run = WorkflowRun::new(WorkflowRunFields {
        run_id: "run-early".into(),
        query_plan_hash: plan_hash.clone(),
        profile_ref: None,
        generation: None,
    })
    .unwrap();
    let answer = sample_grounded_answer(&plan_hash, "ctx");
    let err = try_publish(run, answer).unwrap_err();
    assert_eq!(err.class_name(), "illegal_transition");
}

#[test]
fn record_stage_rejects_illegal_jump() {
    let plan = sample_query_plan();
    let mut run = WorkflowRun::new(WorkflowRunFields {
        run_id: "run-jump".into(),
        query_plan_hash: plan.header.identity.content_hash.as_str().into(),
        profile_ref: None,
        generation: None,
    })
    .unwrap();
    let err = run
        .record_stage(WorkflowStageRecord {
            stage: WorkflowStage::Rendering,
            artifact_hash: None,
            model_fingerprint: None,
            cost: None,
            ok: true,
            detail: None,
        })
        .unwrap_err();
    assert_eq!(err.class_name(), "illegal_transition");
}

#[test]
fn publish_from_non_rendering_fails_on_run() {
    let plan = sample_query_plan();
    let plan_hash = plan.header.identity.content_hash.as_str().to_string();
    let mut run = WorkflowRun::new(WorkflowRunFields {
        run_id: "run-pub-stage".into(),
        query_plan_hash: plan_hash.clone(),
        profile_ref: None,
        generation: None,
    })
    .unwrap();
    run.record_stage(WorkflowStageRecord {
        stage: WorkflowStage::Retrieving,
        artifact_hash: Some("ep-hash".into()),
        model_fingerprint: None,
        cost: None,
        ok: true,
        detail: None,
    })
    .unwrap();
    let answer = sample_grounded_answer(&plan_hash, "cp-hash");
    let err = run.publish(&answer).unwrap_err();
    assert_eq!(err.class_name(), "illegal_transition");
}

#[test]
fn publish_rejects_mismatched_query_plan_hash() {
    let plan = sample_query_plan();
    let plan_hash = plan.header.identity.content_hash.as_str().to_string();
    let mut run = WorkflowRun::new(WorkflowRunFields {
        run_id: "run-digest".into(),
        query_plan_hash: plan_hash.clone(),
        profile_ref: None,
        generation: None,
    })
    .unwrap();
    for stage in [
        WorkflowStage::Retrieving,
        WorkflowStage::Assembling,
        WorkflowStage::Generating,
        WorkflowStage::Verifying,
        WorkflowStage::Rendering,
    ] {
        let artifact_hash = match stage {
            WorkflowStage::Retrieving => Some("ep-hash".into()),
            WorkflowStage::Assembling => Some("cp-hash".into()),
            WorkflowStage::Generating => Some("ap-hash".into()),
            _ => None,
        };
        run.record_stage(WorkflowStageRecord {
            stage,
            artifact_hash,
            model_fingerprint: None,
            cost: None,
            ok: true,
            detail: None,
        })
        .unwrap();
    }
    let answer = sample_grounded_answer("other-query-plan-hash", "cp-hash");
    let err = try_publish(run, answer).unwrap_err();
    assert_eq!(err.class_name(), "validation");
    assert!(
        err.to_string().contains("query_plan_hash"),
        "expected plan hash binding failure, got {err}"
    );
}

#[test]
fn workflow_run_projects_to_wire_envelope() {
    let plan = sample_query_plan();
    let mut run = WorkflowRun::new(WorkflowRunFields {
        run_id: "run-wire".into(),
        query_plan_hash: plan.header.identity.content_hash.as_str().into(),
        profile_ref: Some("profile:default".into()),
        generation: Some("gen-1".into()),
    })
    .unwrap();
    run.record_stage(WorkflowStageRecord {
        stage: WorkflowStage::Retrieving,
        artifact_hash: Some("ep-hash".into()),
        model_fingerprint: None,
        cost: None,
        ok: true,
        detail: None,
    })
    .unwrap();
    let env = workflow_run_to_envelope(&run, "wf-ga-1").unwrap();
    env.validate().unwrap();
    assert_eq!(env.phase, WorkflowPhase::Retrieving);
    assert_eq!(
        env.query_plan_hash,
        plan.header.identity.content_hash.as_str()
    );
}

// ---------------------------------------------------------------------------
// Policy / errors
// ---------------------------------------------------------------------------

#[test]
fn policy_deny_requires_reason_and_maps_error() {
    let gate = PolicyGate::new(
        PolicyGateKind::Budget,
        "token_budget",
        WorkflowStage::Generating,
    )
    .unwrap();
    let denied = PolicyDecision::deny(gate, "budget exhausted");
    let err = denied.into_result().unwrap_err();
    assert_eq!(err.class_name(), "policy_denied");
}

#[test]
fn policy_allow_roundtrip() {
    let gate = PolicyGate::new(PolicyGateKind::Intent, "intent", WorkflowStage::Planned).unwrap();
    let decision = PolicyDecision::allow(gate);
    decision.clone().into_result().unwrap();
    let bytes = encode_wire_document(&decision).unwrap();
    let back: PolicyDecision = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(back, decision);
}

#[test]
fn workflow_error_classes_are_named() {
    let classes = [
        WorkflowError::validation("x").class_name(),
        WorkflowError::illegal_transition(WorkflowStage::Planned, WorkflowStage::Published, "x")
            .class_name(),
        WorkflowError::policy_denied("g", "x").class_name(),
        WorkflowError::verification_failed("x").class_name(),
        WorkflowError::model_failure("x").class_name(),
        WorkflowError::missing_evidence("x").class_name(),
        WorkflowError::budget_exhausted("x").class_name(),
        WorkflowError::disabled("x").class_name(),
    ];
    assert_eq!(classes.len(), 8);
    assert!(classes.iter().all(|c| !c.is_empty()));
}

#[test]
fn empty_hashes_rejected() {
    assert!(WorkflowRun::new(WorkflowRunFields {
        run_id: "r".into(),
        query_plan_hash: " ".into(),
        profile_ref: None,
        generation: None,
    })
    .is_err());
    assert!(ClaimId::new("").is_err());
    assert!(AnswerPlan::new(AnswerPlanFields {
        plan_id: "p".into(),
        context_pack_hash: "".into(),
        instruction: "i".into(),
        allowed_evidence_unit_ids: vec!["eu".into()],
        max_claims: 1,
        model_fingerprint: None,
    })
    .is_err());
}

#[test]
fn schema_version_constant() {
    assert_eq!(GROUNDED_ANSWER_WORKFLOW_SCHEMA_VERSION, 1);
    assert_eq!(
        GROUNDED_ANSWER_CONTRACT_SCHEMA_VERSION,
        GROUNDED_ANSWER_WORKFLOW_SCHEMA_VERSION
    );
}

#[test]
fn model_failure_requires_abstention() {
    let err = WorkflowError::model_failure("malformed schema");
    assert!(err.requires_abstention());
    let err = WorkflowError::verification_failed("unsupported claim");
    assert!(err.requires_abstention());
}
