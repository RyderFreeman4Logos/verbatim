use crate::api::{
    CheckStaleResponse, ChunkingProfileStatusResponse, EmbeddingCapabilityStatusResponse,
    IndexStatusResponse, IndexStatusResponseFields,
};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, ContentHash, WireArtifactKind, WIRE_SCHEMA_VERSION,
};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Serialize)]
struct CheckStaleResultBody<'a> {
    stale: &'a [String],
    #[serde(default, skip_serializing_if = "Option::is_none")]
    profile_status: Option<&'a IndexStatusResponse>,
}

fn profile_status() -> IndexStatusResponse {
    IndexStatusResponse::new(IndexStatusResponseFields(
        true,
        "openai:text-embedding-3-small".into(),
        4,
        1,
        vec!["src-profile".into()],
        EmbeddingCapabilityStatusResponse {
            provider: "openai-compatible".into(),
            model: "text-embedding-3-small".into(),
            dimension: 1536,
            normalize: true,
            endpoint_identity: Some("https://embeddings.local/v1".into()),
            requested_model: Some("text-embedding-3-small".into()),
            served_model: Some("text-embedding-3-small@2026-06".into()),
            max_context_tokens: Some(8192),
            dtype: Some("float16".into()),
            quantization: Some("fp16".into()),
            weight_identity: Some("sha256:weights".into()),
        },
        ChunkingProfileStatusResponse {
            version: "markdown-v1".into(),
            child_target_tokens: 512,
            child_overlap_tokens: 64,
            parent_children_count: 4,
            embedding_input_budget_tokens: Some(7168),
        },
        vec!["embedding status message".into()],
    ))
    .expect("index status response fixture")
}

fn check_stale_response(stale: Vec<String>, with_profile_status: bool) -> CheckStaleResponse {
    CheckStaleResponse::new(stale, with_profile_status.then(profile_status))
        .expect("check stale response fixture")
}

fn encoded(response: &CheckStaleResponse) -> Value {
    serde_json::to_value(response).expect("check stale response encodes")
}

fn assert_rejects(mut wire: Value, mutate: impl FnOnce(&mut Value)) {
    mutate(&mut wire);
    assert!(
        serde_json::from_value::<CheckStaleResponse>(wire).is_err(),
        "mutated check stale response must be rejected during decoding"
    );
}

#[test]
fn check_stale_result_round_trips_empty_and_populated_fixtures() {
    for (stale, with_profile_status) in [
        (Vec::new(), false),
        (vec!["src-1".into()], false),
        (Vec::new(), true),
        (vec!["src-1".into(), "src-2".into()], true),
    ] {
        let response = check_stale_response(stale, with_profile_status);
        let wire = encoded(&response);
        assert_eq!(wire["identity"]["kind"], "check_stale_result");
        assert_eq!(
            wire["identity"]["schema_version"],
            json!({"major": 1, "minor": 0, "patch": 0})
        );
        assert_eq!(wire["identity"]["artifact_id"], "sources-check");
        assert_eq!(
            serde_json::from_value::<CheckStaleResponse>(wire)
                .expect("check stale response decodes"),
            response
        );
    }
}

#[test]
fn check_stale_result_hashes_exact_public_body() {
    let response = check_stale_response(vec!["src-1".into()], true);
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::CheckStaleResult,
        WIRE_SCHEMA_VERSION,
        "sources-check",
        &encode_wire_document(&CheckStaleResultBody {
            stale: &response.stale,
            profile_status: response.profile_status.as_ref(),
        })
        .expect("canonical check stale body encodes"),
    )
    .expect("canonical check stale identity builds");

    assert_eq!(response.identity, expected);
}

#[test]
fn check_stale_result_rejects_missing_identity() {
    let mut wire = encoded(&check_stale_response(vec!["src-1".into()], true));
    wire.as_object_mut()
        .expect("check stale response is an object")
        .remove("identity");

    assert!(serde_json::from_value::<CheckStaleResponse>(wire).is_err());
}

#[test]
fn check_stale_result_rejects_mutated_public_and_nested_fields() {
    let wire = encoded(&check_stale_response(vec!["src-1".into()], true));
    assert_rejects(wire.clone(), |wire| wire["stale"] = json!(["src-other"]));
    assert_rejects(wire.clone(), |wire| {
        wire["profile_status"]["active_profile_id"] = json!("other-profile")
    });
    assert_rejects(wire, |wire| {
        wire["profile_status"]["identity"]["kind"] = json!("index_gc_result")
    });
}

#[test]
fn check_stale_result_rejects_mutated_identity_fields() {
    let wire = encoded(&check_stale_response(vec!["src-1".into()], true));
    assert_rejects(wire.clone(), |wire| {
        wire["identity"]["kind"] = json!("index_gc_result")
    });
    assert_rejects(wire.clone(), |wire| {
        wire["identity"]["schema_version"] = json!({"major": 1, "minor": 0, "patch": 1})
    });
    assert_rejects(wire.clone(), |wire| {
        wire["identity"]["artifact_id"] = json!("other-artifact")
    });
    assert_rejects(wire, |wire| {
        wire["identity"]["content_hash"] = json!("changed")
    });
}

#[test]
fn check_stale_result_rejects_mutation_during_serialization() {
    let mut response = check_stale_response(vec!["src-1".into()], true);
    response.stale[0] = "src-other".into();
    assert!(serde_json::to_value(response).is_err());

    let mut response = check_stale_response(vec!["src-1".into()], true);
    response
        .profile_status
        .as_mut()
        .expect("profile status fixture")
        .active_profile_id = "other-profile".into();
    assert!(serde_json::to_value(response).is_err());

    let mut response = check_stale_response(vec!["src-1".into()], true);
    response.identity.kind = WireArtifactKind::IndexGcResult;
    assert!(serde_json::to_value(response).is_err());

    let mut response = check_stale_response(vec!["src-1".into()], true);
    response.identity.artifact_id = "other-artifact".into();
    assert!(serde_json::to_value(response).is_err());

    let mut response = check_stale_response(vec!["src-1".into()], true);
    response.identity.content_hash = ContentHash::new("changed").expect("content hash fixture");
    assert!(serde_json::to_value(response).is_err());
}
