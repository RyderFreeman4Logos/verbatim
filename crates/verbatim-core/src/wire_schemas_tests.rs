//! Contract tests for versioned R/A/G wire schema envelopes (API-002 / #353).

use super::*;

fn sample_query_plan() -> QueryPlanEnvelope {
    QueryPlanEnvelope::new(QueryPlanFields {
        artifact_id: "qp-1".into(),
        query_text: "what is the retention policy?".into(),
        steps: vec!["lexical".into(), "vector".into()],
        generation: Some("gen-1".into()),
        profile_ref: Some("profile:default".into()),
    })
    .unwrap()
}

fn sample_evidence_pack(plan_hash: &str) -> EvidencePackEnvelope {
    EvidencePackEnvelope::new(EvidencePackFields {
        artifact_id: "ep-1".into(),
        evidence_unit_ids: vec!["eu-a".into(), "eu-b".into()],
        query_plan_hash: plan_hash.into(),
        generation: Some("gen-1".into()),
        profile_ref: Some("profile:default".into()),
    })
    .unwrap()
}

fn sample_context_pack(evidence_hash: &str) -> ContextPackEnvelope {
    ContextPackEnvelope::new(ContextPackFields {
        artifact_id: "cp-1".into(),
        evidence_pack_hash: evidence_hash.into(),
        selected_unit_ids: vec!["eu-a".into()],
        model_fingerprint: Some("model-fp-1".into()),
        generation: Some("gen-1".into()),
        profile_ref: Some("profile:default".into()),
    })
    .unwrap()
}

fn sample_derived(source_hash: &str) -> DerivedArtifactEnvelope {
    DerivedArtifactEnvelope::new(DerivedArtifactFields {
        artifact_id: "da-1".into(),
        kind: DerivedArtifactKind::DraftAnswer,
        source_pack_hash: source_hash.into(),
        model_fingerprint: "model-fp-1".into(),
        generation: None,
        profile_ref: None,
    })
    .unwrap()
}

fn sample_workflow(plan_hash: &str) -> WorkflowEnvelope {
    WorkflowEnvelope::new(WorkflowEnvelopeFields {
        artifact_id: "wf-1".into(),
        phase: WorkflowPhase::Retrieving,
        query_plan_hash: plan_hash.into(),
        evidence_pack_hash: None,
        context_pack_hash: None,
        generation: Some("gen-1".into()),
        profile_ref: Some("profile:default".into()),
    })
    .unwrap()
}

#[test]
fn wire_schema_version_current_is_supported() {
    assert!(WIRE_SCHEMA_VERSION.is_supported());
    validate_wire_schema_version(WIRE_SCHEMA_VERSION).unwrap();
    assert_eq!(WIRE_SCHEMA_VERSION.as_dotted(), "1.0.0");
}

#[test]
fn construction_assigns_kind_and_content_hash() {
    let plan = sample_query_plan();
    assert_eq!(plan.header.schema_version, WIRE_SCHEMA_VERSION);
    assert_eq!(plan.header.identity.kind, WireArtifactKind::QueryPlan);
    assert!(!plan.header.identity.content_hash.as_str().is_empty());
    plan.validate().unwrap();

    let ep = sample_evidence_pack(plan.header.identity.content_hash.as_str());
    assert_eq!(ep.header.identity.kind, WireArtifactKind::EvidencePack);
    ep.validate().unwrap();

    let cp = sample_context_pack(ep.header.identity.content_hash.as_str());
    assert_eq!(cp.header.identity.kind, WireArtifactKind::ContextPack);
    cp.validate().unwrap();

    let da = sample_derived(cp.header.identity.content_hash.as_str());
    assert_eq!(da.header.identity.kind, WireArtifactKind::DerivedArtifact);
    da.validate().unwrap();

    let wf = sample_workflow(plan.header.identity.content_hash.as_str());
    assert_eq!(wf.header.identity.kind, WireArtifactKind::WorkflowEnvelope);
    wf.validate().unwrap();
}

#[test]
fn golden_style_roundtrip_all_envelopes() {
    let plan = sample_query_plan();
    let plan_bytes = encode_wire_document(&plan).unwrap();
    let plan_again = encode_wire_document(&plan).unwrap();
    assert_eq!(
        plan_bytes, plan_again,
        "QueryPlan encode must be byte-stable"
    );
    let plan_back = decode_query_plan_envelope_json(&plan_bytes).unwrap();
    assert_eq!(plan_back, plan);

    let ep = sample_evidence_pack(plan.header.identity.content_hash.as_str());
    let ep_bytes = encode_wire_document(&ep).unwrap();
    assert_eq!(ep_bytes, encode_wire_document(&ep).unwrap());
    let ep_back = decode_evidence_pack_envelope_json(&ep_bytes).unwrap();
    assert_eq!(ep_back, ep);

    let cp = sample_context_pack(ep.header.identity.content_hash.as_str());
    let cp_bytes = encode_wire_document(&cp).unwrap();
    assert_eq!(cp_bytes, encode_wire_document(&cp).unwrap());
    let cp_back = decode_context_pack_envelope_json(&cp_bytes).unwrap();
    assert_eq!(cp_back, cp);

    let da = sample_derived(cp.header.identity.content_hash.as_str());
    let da_bytes = encode_wire_document(&da).unwrap();
    assert_eq!(da_bytes, encode_wire_document(&da).unwrap());
    let da_back = decode_derived_artifact_envelope_json(&da_bytes).unwrap();
    assert_eq!(da_back, da);

    let wf = sample_workflow(plan.header.identity.content_hash.as_str());
    let wf_bytes = encode_wire_document(&wf).unwrap();
    assert_eq!(wf_bytes, encode_wire_document(&wf).unwrap());
    let wf_back = decode_workflow_envelope_json(&wf_bytes).unwrap();
    assert_eq!(wf_back, wf);
}

#[test]
fn unknown_schema_version_fails_closed_on_decode() {
    let mut plan = sample_query_plan();
    plan.header.schema_version = WireSchemaVersion::new(99, 0, 0);
    plan.header.identity.schema_version = WireSchemaVersion::new(99, 0, 0);
    let bytes = encode_wire_document(&plan).unwrap();
    let err = decode_query_plan_envelope_json(&bytes).expect_err("must fail closed");
    assert!(err.to_string().contains("unsupported"), "{err}");

    let mut ep = sample_evidence_pack("abc123deadbeef01");
    ep.header.schema_version = WireSchemaVersion::new(2, 0, 0);
    ep.header.identity.schema_version = WireSchemaVersion::new(2, 0, 0);
    let ep_bytes = encode_wire_document(&ep).unwrap();
    let err = decode_evidence_pack_envelope_json(&ep_bytes).expect_err("must fail closed");
    assert!(err.to_string().contains("unsupported"), "{err}");

    let mut cp = sample_context_pack("abc123deadbeef01");
    cp.header.schema_version = WireSchemaVersion::new(0, 9, 0);
    cp.header.identity.schema_version = WireSchemaVersion::new(0, 9, 0);
    let cp_bytes = encode_wire_document(&cp).unwrap();
    let err = decode_context_pack_envelope_json(&cp_bytes).expect_err("must fail closed");
    assert!(err.to_string().contains("unsupported"), "{err}");

    let mut da = sample_derived("abc123deadbeef01");
    da.header.schema_version = WireSchemaVersion::new(3, 1, 0);
    da.header.identity.schema_version = WireSchemaVersion::new(3, 1, 0);
    let da_bytes = encode_wire_document(&da).unwrap();
    let err = decode_derived_artifact_envelope_json(&da_bytes).expect_err("must fail closed");
    assert!(err.to_string().contains("unsupported"), "{err}");

    let mut wf = sample_workflow("abc123deadbeef01");
    wf.header.schema_version = WireSchemaVersion::new(9, 9, 9);
    wf.header.identity.schema_version = WireSchemaVersion::new(9, 9, 9);
    let wf_bytes = encode_wire_document(&wf).unwrap();
    let err = decode_workflow_envelope_json(&wf_bytes).expect_err("must fail closed");
    assert!(err.to_string().contains("unsupported"), "{err}");
}

#[test]
fn invalid_identity_and_hash_rejected() {
    let err = CanonicalIdentity::new(CanonicalIdentityFields {
        kind: WireArtifactKind::QueryPlan,
        schema_version: WIRE_SCHEMA_VERSION,
        artifact_id: "  ".into(),
        content_hash: "abc123deadbeef01".into(),
    })
    .expect_err("empty artifact_id");
    assert!(err.to_string().contains("artifact_id"), "{err}");

    let err = ContentHash::new("").expect_err("empty hash");
    assert!(err.to_string().contains("empty"), "{err}");

    let err = ContentHash::new("has space").expect_err("whitespace hash");
    assert!(err.to_string().contains("whitespace"), "{err}");

    let err = QueryPlanEnvelope::new(QueryPlanFields {
        artifact_id: "qp".into(),
        query_text: "".into(),
        steps: vec![],
        generation: None,
        profile_ref: None,
    })
    .expect_err("empty query");
    assert!(err.to_string().contains("query_text"), "{err}");

    let err = EvidencePackEnvelope::new(EvidencePackFields {
        artifact_id: "ep".into(),
        evidence_unit_ids: vec![],
        query_plan_hash: "abc".into(),
        generation: None,
        profile_ref: None,
    })
    .expect_err("empty units");
    assert!(err.to_string().contains("evidence_unit_ids"), "{err}");

    let err = EvidencePackEnvelope::new(EvidencePackFields {
        artifact_id: "ep".into(),
        evidence_unit_ids: vec!["eu".into()],
        query_plan_hash: "bad hash".into(),
        generation: None,
        profile_ref: None,
    })
    .expect_err("whitespace hash");
    assert!(err.to_string().contains("whitespace"), "{err}");

    let err = ContextPackEnvelope::new(ContextPackFields {
        artifact_id: "cp".into(),
        evidence_pack_hash: "".into(),
        selected_unit_ids: vec!["eu".into()],
        model_fingerprint: None,
        generation: None,
        profile_ref: None,
    })
    .expect_err("empty evidence hash");
    assert!(err.to_string().contains("evidence_pack_hash"), "{err}");

    let err = DerivedArtifactEnvelope::new(DerivedArtifactFields {
        artifact_id: "da".into(),
        kind: DerivedArtifactKind::Summary,
        source_pack_hash: "okhash".into(),
        model_fingerprint: "  ".into(),
        generation: None,
        profile_ref: None,
    })
    .expect_err("empty model fingerprint");
    assert!(err.to_string().contains("model_fingerprint"), "{err}");

    let err = WorkflowEnvelope::new(WorkflowEnvelopeFields {
        artifact_id: "wf".into(),
        phase: WorkflowPhase::Failed,
        query_plan_hash: "".into(),
        evidence_pack_hash: None,
        context_pack_hash: None,
        generation: None,
        profile_ref: None,
    })
    .expect_err("empty plan hash");
    assert!(err.to_string().contains("query_plan_hash"), "{err}");
}

#[test]
fn tampered_content_hash_fails_validation() {
    let mut plan = sample_query_plan();
    plan.header.identity.content_hash = ContentHash::new("deadbeefdeadbeef").unwrap();
    let err = plan.validate().expect_err("hash must match body");
    assert!(err.to_string().contains("content hash mismatch"), "{err}");

    let bytes = encode_wire_document(&plan).unwrap();
    let err = decode_query_plan_envelope_json(&bytes).expect_err("decode must revalidate hash");
    assert!(err.to_string().contains("content hash mismatch"), "{err}");
}

#[test]
fn kind_mismatch_fails_validation() {
    let mut plan = sample_query_plan();
    plan.header.identity.kind = WireArtifactKind::EvidencePack;
    let err = plan.validate().expect_err("kind must match envelope");
    assert!(err.to_string().contains("query_plan"), "{err}");
}

#[test]
fn empty_optional_generation_rejected() {
    let plan = sample_query_plan();
    let err = WireEnvelopeHeader::new(WireEnvelopeHeaderFields {
        identity: plan.header.identity.clone(),
        generation: Some(" ".into()),
        profile_ref: None,
    })
    .expect_err("empty generation");
    assert!(err.to_string().contains("generation"), "{err}");
}

#[test]
fn content_hash_helper_is_deterministic() {
    let body = br#"{"a":1}"#;
    assert_eq!(wire_content_hash(body), wire_content_hash(body));
    assert_ne!(wire_content_hash(body), wire_content_hash(br#"{"a":2}"#));
}
