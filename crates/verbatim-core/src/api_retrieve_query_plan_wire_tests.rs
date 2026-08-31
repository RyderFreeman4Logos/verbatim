use super::api_retrieve_envelope_wire_tests::{
    sample_ask_context_response, sample_retrieve_response,
};
use super::{AskResponse, RetrieveResponse};
use crate::wire_schemas::{QueryPlanEnvelope, WireArtifactKind};

fn tampered_query_plan(mut plan: QueryPlanEnvelope, tamper: &str) -> QueryPlanEnvelope {
    match tamper {
        "body_hash" => plan.query_text.push_str(" tampered"),
        "kind" => plan.header.identity.kind = WireArtifactKind::EvidencePack,
        "artifact_id" => plan.header.identity.artifact_id = "other-retrieve".into(),
        _ => unreachable!("unknown QueryPlan tamper: {tamper}"),
    }
    plan
}

fn tamper_query_plan_value(plan: &mut serde_json::Value, tamper: &str) {
    match tamper {
        "body_hash" => plan["query_text"] = serde_json::json!("What is cited? tampered"),
        "kind" => plan["header"]["identity"]["kind"] = serde_json::json!("evidence_pack"),
        "artifact_id" => {
            plan["header"]["identity"]["artifact_id"] = serde_json::json!("other-retrieve")
        }
        _ => unreachable!("unknown QueryPlan tamper: {tamper}"),
    }
}

fn retrieve_response_with_tampered_plan(tamper: &str) -> RetrieveResponse {
    let mut response = sample_retrieve_response("What is cited?", "ev-1");
    let encoded = serde_json::to_value(&response).unwrap();
    let plan = serde_json::from_value(encoded["query_plan"].clone()).unwrap();
    response.query_plan = Some(tampered_query_plan(plan, tamper));
    response
}

fn ask_response_with_tampered_retrieve_plan(tamper: &str) -> AskResponse {
    let mut response = sample_ask_context_response("ev-1");
    let encoded = serde_json::to_value(&response).unwrap();
    let plan = serde_json::from_value(encoded["context"]["query_plan"].clone()).unwrap();
    response.context.as_mut().unwrap().query_plan = Some(tampered_query_plan(plan, tamper));
    response
}

fn encoded_retrieve_response_with_tampered_plan(tamper: &str) -> serde_json::Value {
    let mut encoded =
        serde_json::to_value(sample_retrieve_response("What is cited?", "ev-1")).unwrap();
    tamper_query_plan_value(&mut encoded["query_plan"], tamper);
    encoded
}

fn encoded_ask_response_with_tampered_retrieve_plan(tamper: &str) -> serde_json::Value {
    let mut encoded = serde_json::to_value(sample_ask_context_response("ev-1")).unwrap();
    tamper_query_plan_value(&mut encoded["context"]["query_plan"], tamper);
    super::with_ask_run_identity(encoded)
}

fn expected_query_plan_tamper_error(tamper: &str) -> &'static str {
    match tamper {
        "body_hash" => "content hash mismatch",
        "kind" => "query plan envelope identity kind must be query_plan",
        "artifact_id" => "retrieve query plan artifact_id must be live-retrieve",
        _ => unreachable!("unknown QueryPlan tamper: {tamper}"),
    }
}

const QUERY_PLAN_TAMPERS: [&str; 3] = ["body_hash", "kind", "artifact_id"];

#[test]
fn live_retrieve_response_serialize_rejects_tampered_query_plan_identity() {
    for tamper in QUERY_PLAN_TAMPERS {
        let error = serde_json::to_value(retrieve_response_with_tampered_plan(tamper))
            .expect_err("tampered retrieve QueryPlan must fail serialization");
        assert!(
            error
                .to_string()
                .contains(expected_query_plan_tamper_error(tamper)),
            "tamper={tamper}, error={error}"
        );
    }
}

#[test]
fn live_retrieve_response_decode_rejects_tampered_query_plan_identity() {
    for tamper in QUERY_PLAN_TAMPERS {
        let error = serde_json::from_value::<RetrieveResponse>(
            encoded_retrieve_response_with_tampered_plan(tamper),
        )
        .expect_err("tampered retrieve QueryPlan must fail decoding");
        assert!(
            error
                .to_string()
                .contains(expected_query_plan_tamper_error(tamper)),
            "tamper={tamper}, error={error}"
        );
    }
}

#[test]
fn live_nested_ask_serialize_rejects_tampered_retrieve_query_plan_identity() {
    for tamper in QUERY_PLAN_TAMPERS {
        let error = serde_json::to_value(ask_response_with_tampered_retrieve_plan(tamper))
            .expect_err("tampered nested retrieve QueryPlan must fail serialization");
        assert!(
            error
                .to_string()
                .contains(expected_query_plan_tamper_error(tamper)),
            "tamper={tamper}, error={error}"
        );
    }
}

#[test]
fn live_nested_ask_decode_rejects_tampered_retrieve_query_plan_identity() {
    for tamper in QUERY_PLAN_TAMPERS {
        let error = serde_json::from_value::<AskResponse>(
            encoded_ask_response_with_tampered_retrieve_plan(tamper),
        )
        .expect_err("tampered nested retrieve QueryPlan must fail decoding");
        assert!(
            error
                .to_string()
                .contains(expected_query_plan_tamper_error(tamper)),
            "tamper={tamper}, error={error}"
        );
    }
}
