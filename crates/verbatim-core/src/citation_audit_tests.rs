use super::*;

fn source_document() -> AuditDocument {
    AuditDocument::new(AuditDocumentFields {
        document_id: "external-answer-1".into(),
        prose: "The policy permits a 30-day refund. It has no cancellation fee.".into(),
        existing_citations: vec![UntrustedExistingCitation {
            source_start: 0,
            source_end: 0,
            raw_citation: "[fabricated: E-99]".into(),
        }],
    })
    .expect("fixture document is valid")
}

fn claim(document: &AuditDocument, id: &str, text: &str) -> ClaimRecord {
    let start = document
        .prose
        .find(text)
        .expect("fixture claim occurs in source prose") as u64;
    ClaimRecord::new(ClaimRecordFields {
        claim_id: id.into(),
        text: text.into(),
        source_start: start,
        source_end: start + text.len() as u64,
        existing_citations: document.existing_citations.clone(),
    })
    .expect("fixture claim is valid")
}

fn evidence_registry() -> EvidenceRegistry {
    EvidenceRegistry::new(vec![
        ResolvedEvidence {
            evidence_id: "E-1".into(),
            source_hash: "a".repeat(64),
            locator: "policy.md#refunds".into(),
            text: "Customers may request a refund within 30 days.".into(),
        },
        ResolvedEvidence {
            evidence_id: "E-2".into(),
            source_hash: "b".repeat(64),
            locator: "policy.md#fees".into(),
            text: "A cancellation fee of $20 applies to every refund.".into(),
        },
    ])
    .expect("fixture evidence is server-resolved")
}

fn reference(id: &str, quotation: &str) -> EvidenceReference {
    EvidenceReference {
        evidence_id: id.into(),
        quotation: quotation.into(),
    }
}

fn result(
    claim_id: &str,
    classification: EvidenceClassification,
    evidence: Vec<EvidenceReference>,
    missing_requirements: Vec<String>,
    conflicts: Vec<ClaimConflict>,
    source_applicability: SourceApplicability,
) -> ClaimAuditResult {
    ClaimAuditResult::new(ClaimAuditResultFields {
        claim_id: claim_id.into(),
        classification,
        evidence,
        missing_requirements,
        conflicts,
        source_applicability,
        confidence: Calibration {
            status: CalibrationStatus::Calibrated,
            basis_points: 8_500,
        },
    })
    .expect("fixture result is structurally valid")
}

#[test]
fn claim_records_bind_stable_ids_and_utf8_safe_source_offsets() {
    let document = source_document();
    let record = claim(&document, "claim-1", "The policy permits a 30-day refund.");
    record
        .validate_for_document(&document)
        .expect("claim text and offsets bind to document");
    assert_eq!(record.claim_id.as_str(), "claim-1");

    let mut split_utf8 = record;
    split_utf8.source_start = 1;
    assert!(matches!(
        split_utf8.validate_for_document(&document),
        Err(CitationAuditError::Validation { .. })
    ));
}

#[test]
fn every_evidence_classification_has_its_required_fail_closed_shape() {
    let document = source_document();
    let first = claim(&document, "claim-1", "The policy permits a 30-day refund.");
    let second = claim(&document, "claim-2", "It has no cancellation fee.");
    let registry = evidence_registry();
    let refund_quote = "Customers may request a refund within 30 days.";
    let fee_quote = "A cancellation fee of $20 applies to every refund.";

    let supported = result(
        "claim-1",
        EvidenceClassification::Supported,
        vec![reference("E-1", refund_quote)],
        vec![],
        vec![],
        SourceApplicability::Applicable,
    );
    let partial = result(
        "claim-1",
        EvidenceClassification::PartiallySupported,
        vec![reference("E-1", refund_quote)],
        vec!["eligibility exceptions are not resolved".into()],
        vec![],
        SourceApplicability::PartiallyApplicable,
    );
    let contradicted = result(
        "claim-2",
        EvidenceClassification::Contradicted,
        vec![reference("E-2", fee_quote)],
        vec!["the claim omits the stated fee".into()],
        vec![ClaimConflict {
            evidence_id: "E-2".into(),
            detail: "The evidence states that a $20 fee applies.".into(),
        }],
        SourceApplicability::Applicable,
    );
    let unrelated = result(
        "claim-1",
        EvidenceClassification::Unrelated,
        vec![reference("E-2", fee_quote)],
        vec!["refund timing is not addressed by this fee evidence".into()],
        vec![],
        SourceApplicability::NotApplicable,
    );
    let insufficient = result(
        "claim-2",
        EvidenceClassification::Insufficient,
        vec![],
        vec!["no evidence addressing whether fees exist".into()],
        vec![],
        SourceApplicability::Unknown,
    );

    for (record, audit) in [
        (&first, &supported),
        (&first, &partial),
        (&second, &contradicted),
        (&first, &unrelated),
        (&second, &insufficient),
    ] {
        audit.validate_for_claim(record, &registry).expect(
            "each documented classification validates only with its required evidence shape",
        );
    }
    assert_eq!(EvidenceClassification::all().len(), 5);
}

#[test]
fn fabricated_or_altered_evidence_is_rejected_and_existing_citations_are_untrusted() {
    let document = source_document();
    let record = claim(&document, "claim-1", "The policy permits a 30-day refund.");
    let registry = evidence_registry();

    let fabricated = result(
        "claim-1",
        EvidenceClassification::Supported,
        vec![reference("E-99", "totally invented")],
        vec![],
        vec![],
        SourceApplicability::Applicable,
    );
    assert!(matches!(
        fabricated.validate_for_claim(&record, &registry),
        Err(CitationAuditError::EvidenceRejected { .. })
    ));

    let altered = result(
        "claim-1",
        EvidenceClassification::Supported,
        vec![reference(
            "E-1",
            "Customers may request a refund within 60 days.",
        )],
        vec![],
        vec![],
        SourceApplicability::Applicable,
    );
    assert!(matches!(
        altered.validate_for_claim(&record, &registry),
        Err(CitationAuditError::EvidenceRejected { .. })
    ));

    assert!(!document.existing_citations.is_empty());
    assert!(matches!(
        fabricated.validate_for_claim(&record, &registry),
        Err(CitationAuditError::EvidenceRejected { .. })
    ));
}

#[test]
fn untrusted_document_or_evidence_text_cannot_control_the_workflow() {
    for origin in [AuditTextOrigin::DocumentBody, AuditTextOrigin::EvidenceText] {
        assert!(!origin.may_alter_workflow_control());
        assert!(matches!(
            guard_workflow_control(origin),
            Err(CitationAuditError::UntrustedControl { .. })
        ));
    }
    assert!(guard_workflow_control(AuditTextOrigin::PolicyConfig).is_ok());
}

#[test]
fn aggregate_coverage_and_run_json_are_hash_bound_and_fail_closed() {
    let document = source_document();
    let record = claim(&document, "claim-1", "The policy permits a 30-day refund.");
    let segmentation = ClaimSegmentation::new(&document, vec![record.clone()])
        .expect("segmentation binds document hash and claim offsets");
    let registry = evidence_registry();
    let supported = result(
        "claim-1",
        EvidenceClassification::Supported,
        vec![reference(
            "E-1",
            "Customers may request a refund within 30 days.",
        )],
        vec![],
        vec![],
        SourceApplicability::Applicable,
    );
    let coverage = ClaimCoverageEnvelope::new(&segmentation, &[supported.clone()], &registry)
        .expect("coverage is computed from validated per-claim results");
    assert_eq!(coverage.status, CoverageStatus::Complete);
    assert_eq!(coverage.counts.supported, 1);

    let mut run = start_run(
        "audit-run-1".into(),
        &document,
        CitationAuditBudget::skeleton_default(),
    )
    .expect("run starts without generation enabled");
    for stage in [
        CitationAuditStage::Retrieving,
        CitationAuditStage::Classifying,
        CitationAuditStage::Validating,
        CitationAuditStage::Aggregating,
    ] {
        advance_stage(&mut run, stage).expect("ordered audit stages advance");
    }
    complete_run(&mut run, &segmentation, &[supported], &coverage, &registry)
        .expect("only validated results and matching coverage complete a run");
    let encoded = encode_citation_audit_run_json(&run).expect("run encodes canonically");
    assert!(
        decode_citation_audit_run_json(&encoded).expect("run decodes") == run,
        "encoded run must round-trip exactly"
    );

    let mut unknown_schema = run;
    unknown_schema.schema_version = 99;
    let malformed = serde_json::to_vec(&unknown_schema).expect("test json serializes");
    assert!(matches!(
        decode_citation_audit_run_json(&malformed),
        Err(CitationAuditError::Validation { .. })
    ));
}

#[test]
fn run_json_decode_rejects_invalid_budget_and_over_cap_usage() {
    let document = source_document();
    let run = start_run(
        "budget-validation-run".into(),
        &document,
        CitationAuditBudget::skeleton_default(),
    )
    .expect("run starts with a valid budget");

    let mut zero_cap = run.clone();
    zero_cap.budget.max_claims = 0;
    let zero_cap_json = serde_json::to_vec(&zero_cap).expect("encode zero-cap run fixture");
    assert!(matches!(
        decode_citation_audit_run_json(&zero_cap_json),
        Err(CitationAuditError::Validation { .. })
    ));

    for (dimension, usage) in [
        (
            CitationAuditBudgetDimension::Claims,
            CitationAuditUsage {
                claims: run.budget.max_claims + 1,
                ..Default::default()
            },
        ),
        (
            CitationAuditBudgetDimension::Candidates,
            CitationAuditUsage {
                candidates: run.budget.max_candidates + 1,
                ..Default::default()
            },
        ),
        (
            CitationAuditBudgetDimension::Classifications,
            CitationAuditUsage {
                classifications: run.budget.max_classifications + 1,
                ..Default::default()
            },
        ),
        (
            CitationAuditBudgetDimension::CostUnits,
            CitationAuditUsage {
                cost_units: run.budget.max_cost_units + 1,
                ..Default::default()
            },
        ),
        (
            CitationAuditBudgetDimension::WallTimeMs,
            CitationAuditUsage {
                wall_time_ms: run.budget.max_wall_time_ms + 1,
                ..Default::default()
            },
        ),
    ] {
        let mut over_cap = run.clone();
        over_cap.usage = usage;
        let over_cap_json = serde_json::to_vec(&over_cap).expect("encode over-cap run fixture");
        assert!(matches!(
            decode_citation_audit_run_json(&over_cap_json),
            Err(CitationAuditError::BudgetExhausted { exhaustion }) if exhaustion.dimension == dimension
        ));
    }
}

#[test]
fn budget_caps_are_checked_before_usage_mutates() {
    let budget = CitationAuditBudget::new(CitationAuditBudgetFields {
        max_claims: 1,
        max_candidates: 1,
        max_classifications: 1,
        max_cost_units: 1,
        max_wall_time_ms: 1,
    })
    .expect("bounded budget");
    let usage = CitationAuditUsage::default();
    let exceeded = usage.checked_add(
        CitationAuditUsage {
            candidates: 2,
            ..Default::default()
        },
        &budget,
    );
    assert!(matches!(
        exceeded,
        Err(CitationAuditError::BudgetExhausted { exhaustion })
            if exhaustion.dimension == CitationAuditBudgetDimension::Candidates
    ));
    assert_eq!(usage, CitationAuditUsage::default());
}
