use super::*;

#[test]
fn live_workflow_wire_valid_envelope_round_trips() {
    let query_plan = sample_query_plan();
    let evidence_pack = sample_evidence_pack(query_plan.header.identity.content_hash.as_str());
    let context_pack = sample_context_pack(evidence_pack.header.identity.content_hash.as_str());
    let request = live_workflow_request(query_plan, Some(evidence_pack), Some(context_pack));

    let request_wire = serde_json::to_value(&request).unwrap();
    assert_eq!(
        request_wire["workflow"]["header"]["identity"]["kind"],
        "workflow_envelope"
    );
    assert_eq!(
        serde_json::from_value::<WorkflowRunRequest>(request_wire).unwrap(),
        request
    );

    let response = WorkflowRunResponse::new("run-sdk-live", request).unwrap();
    let response_wire = serde_json::to_value(&response).unwrap();
    assert_eq!(
        serde_json::from_value::<WorkflowRunResponse>(response_wire).unwrap(),
        response
    );
}

#[test]
fn live_workflow_wire_mismatched_payload_vs_envelope_is_rejected() {
    let query_plan = sample_query_plan();
    let other_query_plan = QueryPlanEnvelope::new(QueryPlanFields {
        artifact_id: "qp-sdk-other".into(),
        query_text: "a different question".into(),
        steps: vec!["lexical".into()],
        generation: None,
        profile_ref: None,
    })
    .unwrap();
    let mismatched = WorkflowRunRequest {
        workflow: sample_workflow(query_plan.header.identity.content_hash.as_str()),
        query_plan: other_query_plan.clone(),
        evidence_pack: None,
        context_pack: None,
        idempotency_key: None,
    };
    serde_json::to_value(mismatched)
        .expect_err("workflow identity must match the executed query plan on serialize");

    let mut encoded = serde_json::to_value(live_workflow_request(query_plan, None, None)).unwrap();
    encoded["query_plan"] = serde_json::to_value(other_query_plan).unwrap();
    serde_json::from_value::<WorkflowRunRequest>(encoded)
        .expect_err("workflow identity must match the executed query plan on deserialize");
}

#[test]
fn live_workflow_wire_noncanonical_or_incomplete_identity_is_rejected() {
    let request = live_workflow_request(sample_query_plan(), None, None);

    let mut noncanonical = serde_json::to_value(&request).unwrap();
    noncanonical["workflow"]["header"]["identity"]["content_hash"] = serde_json::json!("different");
    serde_json::from_value::<WorkflowRunRequest>(noncanonical)
        .expect_err("noncanonical workflow identity must fail closed");

    let mut incomplete = serde_json::to_value(&request).unwrap();
    incomplete["workflow"]["header"]["identity"]
        .as_object_mut()
        .unwrap()
        .remove("content_hash");
    serde_json::from_value::<WorkflowRunRequest>(incomplete)
        .expect_err("incomplete workflow identity must fail closed");

    let mut unknown = serde_json::to_value(request).unwrap();
    unknown["workflow"]["header"]["identity"]["kind"] = serde_json::json!("unknown");
    serde_json::from_value::<WorkflowRunRequest>(unknown)
        .expect_err("unknown workflow identity kind must fail closed");
}

#[test]
fn live_workflow_wire_mixed_blank_unit_ids_serialize_is_rejected() {
    let request = workflow_request_with_context_ids(vec!["eu-a".into(), " ".into()]);
    assert!(request.evidence_pack.is_none());
    serde_json::to_value(request)
        .expect_err("mixed blank context unit ids must fail closed on serialize");
}

#[test]
fn live_workflow_wire_mixed_blank_unit_ids_deserialize_is_rejected() {
    let request = workflow_request_with_context_ids(vec!["eu-a".into(), " ".into()]);
    assert!(request.evidence_pack.is_none());
    let encoded = serde_json::json!({
        "workflow": request.workflow,
        "query_plan": request.query_plan,
        "context_pack": request.context_pack,
    });
    serde_json::from_value::<WorkflowRunRequest>(encoded)
        .expect_err("mixed blank context unit ids must fail closed on deserialize");
}

#[test]
fn live_workflow_wire_all_blank_unit_ids_serialize_is_rejected() {
    let request = workflow_request_with_context_ids(vec![" ".into()]);
    assert!(request.evidence_pack.is_none());
    serde_json::to_value(request)
        .expect_err("all blank context unit ids must fail closed on serialize");
}

#[test]
fn live_workflow_wire_all_blank_unit_ids_deserialize_is_rejected() {
    let request = workflow_request_with_context_ids(vec![" ".into()]);
    assert!(request.evidence_pack.is_none());
    let encoded = serde_json::json!({
        "workflow": request.workflow,
        "query_plan": request.query_plan,
        "context_pack": request.context_pack,
    });
    serde_json::from_value::<WorkflowRunRequest>(encoded)
        .expect_err("all blank context unit ids must fail closed on deserialize");
}
