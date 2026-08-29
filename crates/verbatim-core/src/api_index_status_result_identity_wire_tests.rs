use crate::api::{
    ChunkingProfileStatusResponse, EmbeddingCapabilityStatusResponse, IndexStatusResponse,
};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};
use serde::Serialize;
use serde_json::{json, Value};

#[derive(Debug, Serialize)]
struct CanonicalStatusBody<'a> {
    embedding_enabled: bool,
    active_profile_id: &'a str,
    source_count: usize,
    stale_source_count: usize,
    stale_source_ids: &'a [String],
    capability: &'a EmbeddingCapabilityStatusResponse,
    chunking: &'a ChunkingProfileStatusResponse,
    messages: &'a [String],
}

fn response(
    embedding_enabled: bool,
    stale_source_ids: Vec<String>,
    optional_capability_and_chunking_fields: bool,
) -> IndexStatusResponse {
    let stale_source_count = stale_source_ids.len();
    IndexStatusResponse::new(
        embedding_enabled,
        if embedding_enabled {
            "openai:text-embedding-3-small".into()
        } else {
            "lexical-only".into()
        },
        4,
        stale_source_count,
        stale_source_ids,
        EmbeddingCapabilityStatusResponse {
            provider: "openai-compatible".into(),
            model: "text-embedding-3-small".into(),
            dimension: 1536,
            normalize: true,
            endpoint_identity: optional_capability_and_chunking_fields
                .then(|| "https://embeddings.local/v1".into()),
            requested_model: optional_capability_and_chunking_fields
                .then(|| "text-embedding-3-small".into()),
            served_model: optional_capability_and_chunking_fields
                .then(|| "text-embedding-3-small@2026-06".into()),
            max_context_tokens: optional_capability_and_chunking_fields.then_some(8192),
            dtype: optional_capability_and_chunking_fields.then(|| "float16".into()),
            quantization: optional_capability_and_chunking_fields.then(|| "fp16".into()),
            weight_identity: optional_capability_and_chunking_fields
                .then(|| "sha256:weights".into()),
        },
        ChunkingProfileStatusResponse {
            version: "markdown-v1".into(),
            child_target_tokens: 512,
            child_overlap_tokens: 64,
            parent_children_count: 4,
            embedding_input_budget_tokens: optional_capability_and_chunking_fields.then_some(7168),
        },
        if embedding_enabled {
            vec!["embedding status message".into()]
        } else {
            Vec::new()
        },
    )
    .expect("index status response fixture")
}

fn encoded(response: &IndexStatusResponse) -> Value {
    serde_json::to_value(response).expect("status response encodes")
}

fn assert_rejects(mut wire: Value, mutate: impl FnOnce(&mut Value)) {
    mutate(&mut wire);
    assert!(
        serde_json::from_value::<IndexStatusResponse>(wire).is_err(),
        "mutated status response must be rejected during decoding"
    );
}

#[test]
fn index_status_result_round_trips_valid_response_variants() {
    for (embedding_enabled, stale_source_ids, optional_fields) in [
        (false, Vec::new(), false),
        (false, vec!["src-1".into()], false),
        (true, Vec::new(), true),
        (true, vec!["src-1".into(), "src-2".into()], true),
    ] {
        let response = response(embedding_enabled, stale_source_ids, optional_fields);
        let wire = encoded(&response);
        assert_eq!(wire["identity"]["kind"], "index_status_result");
        assert_eq!(
            wire["identity"]["schema_version"],
            json!({"major": 1, "minor": 0, "patch": 0})
        );
        assert_eq!(wire["identity"]["artifact_id"], "index-status");
        assert_eq!(
            serde_json::from_value::<IndexStatusResponse>(wire).expect("status response decodes"),
            response
        );
    }
}

#[test]
fn index_status_result_hashes_exact_public_body() {
    let response = response(true, vec!["src-1".into()], true);
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::IndexStatusResult,
        WIRE_SCHEMA_VERSION,
        "index-status",
        &encode_wire_document(&CanonicalStatusBody {
            embedding_enabled: response.embedding_enabled,
            active_profile_id: &response.active_profile_id,
            source_count: response.source_count,
            stale_source_count: response.stale_source_count,
            stale_source_ids: &response.stale_source_ids,
            capability: &response.capability,
            chunking: &response.chunking,
            messages: &response.messages,
        })
        .expect("canonical status body encodes"),
    )
    .expect("canonical status identity builds");

    assert_eq!(response.identity, expected);
}

#[test]
fn index_status_result_rejects_missing_identity() {
    let mut wire = encoded(&response(true, vec!["src-1".into()], true));
    wire.as_object_mut()
        .expect("status response is an object")
        .remove("identity");

    assert!(serde_json::from_value::<IndexStatusResponse>(wire).is_err());
}

#[test]
fn index_status_result_rejects_mutated_public_fields() {
    let wire = encoded(&response(true, vec!["src-1".into()], true));
    for (field, value) in [
        ("embedding_enabled", json!(false)),
        ("active_profile_id", json!("other-profile")),
        ("source_count", json!(99)),
        ("stale_source_count", json!(99)),
        ("stale_source_ids", json!(["src-other"])),
        ("messages", json!(["other message"])),
    ] {
        assert_rejects(wire.clone(), |wire| wire[field] = value);
    }
}

#[test]
fn index_status_result_rejects_mutated_capability_and_chunking_fields() {
    let wire = encoded(&response(true, vec!["src-1".into()], true));
    for (field, value) in [
        ("provider", json!("other-provider")),
        ("model", json!("other-model")),
        ("dimension", json!(1)),
        ("normalize", json!(false)),
        ("endpoint_identity", json!("other-endpoint")),
        ("requested_model", json!("other-requested-model")),
        ("served_model", json!("other-served-model")),
        ("max_context_tokens", json!(1)),
        ("dtype", json!("other-dtype")),
        ("quantization", json!("other-quantization")),
        ("weight_identity", json!("other-weight")),
    ] {
        assert_rejects(wire.clone(), |wire| wire["capability"][field] = value);
    }
    for (field, value) in [
        ("version", json!("other-version")),
        ("child_target_tokens", json!(1)),
        ("child_overlap_tokens", json!(1)),
        ("parent_children_count", json!(1)),
        ("embedding_input_budget_tokens", json!(1)),
    ] {
        assert_rejects(wire.clone(), |wire| wire["chunking"][field] = value);
    }
}

#[test]
fn index_status_result_rejects_mutated_identity_fields() {
    let wire = encoded(&response(true, vec!["src-1".into()], true));
    assert_rejects(wire.clone(), |wire| {
        wire["identity"]["kind"] = json!("index_gc_result")
    });
    assert_rejects(wire.clone(), |wire| {
        wire["identity"]["schema_version"] = json!("1.0.1")
    });
    assert_rejects(wire.clone(), |wire| {
        wire["identity"]["artifact_id"] = json!("other-artifact")
    });
    assert_rejects(wire, |wire| {
        wire["identity"]["content_hash"] = json!("changed")
    });
}

#[test]
fn index_status_result_rejects_mutation_during_serialization() {
    let mut response = response(true, vec!["src-1".into()], true);
    response.source_count = 99;
    assert!(serde_json::to_value(response).is_err());
}
