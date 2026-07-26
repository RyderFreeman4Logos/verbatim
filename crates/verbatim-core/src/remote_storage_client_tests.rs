//! Unit/contract tests for remote storage client walking skeleton.

use super::*;
use crate::observability_contract::{TraceContext, TraceContextFields};
use crate::storage_ports::{
    StorageCapabilityKind, StorageError, StorageGeneration, StoragePrincipal,
    STORAGE_PORTS_SCHEMA_VERSION,
};

fn service_identity() -> RemoteClientIdentity {
    RemoteClientIdentity::service("coord-1", ServiceRole::Writer).expect("identity")
}

fn read_op() -> RemoteOperation {
    RemoteOperation::read(StorageCapabilityKind::CatalogStore, "list_sources").expect("op")
}

fn mutate_op() -> RemoteOperation {
    RemoteOperation::mutation(StorageCapabilityKind::EvidenceStore, "put_evidence").expect("op")
}

fn read_retry() -> RetryPolicy {
    RetryPolicy::for_operation(MutationKind::Read, None).expect("retry")
}

fn upsert_retry(key: &str) -> RetryPolicy {
    RetryPolicy::for_operation(
        MutationKind::Upsert,
        Some(IdempotencyKey::new(key).expect("key")),
    )
    .expect("retry")
}

// ---------------------------------------------------------------------------
// Unauthorized cannot enumerate / fetch
// ---------------------------------------------------------------------------

#[test]
fn unauthenticated_cannot_enumerate_or_fetch() {
    let identity = RemoteClientIdentity::unauthenticated();
    assert!(identity.is_unauthenticated());
    let err = identity
        .require_authenticated("list_sources")
        .expect_err("must refuse");
    assert!(matches!(err, StorageError::Unauthorized { .. }));
    assert!(err.to_string().contains("unauthenticated"));

    let fetch_err = identity
        .require_authenticated("get_blob")
        .expect_err("must refuse fetch");
    assert!(matches!(fetch_err, StorageError::Unauthorized { .. }));

    let envelope = RemoteRequestEnvelope::new(
        identity,
        read_op(),
        RequestBounds::test_defaults(),
        read_retry(),
    )
    .expect("envelope validates structure");
    let preflight = envelope.authorize_preflight().expect_err("preflight");
    assert!(matches!(preflight, StorageError::Unauthorized { .. }));
}

#[test]
fn reader_cannot_mutate() {
    let identity = RemoteClientIdentity::token(ServiceRole::Reader);
    let err = identity
        .require_mutation("put_evidence")
        .expect_err("reader blocked");
    assert!(matches!(err, StorageError::Unauthorized { .. }));

    let envelope = RemoteRequestEnvelope::new(
        identity,
        mutate_op(),
        RequestBounds::test_defaults(),
        upsert_retry("k1"),
    )
    .expect("envelope");
    let preflight = envelope.authorize_preflight().expect_err("mutate");
    assert!(matches!(preflight, StorageError::Unauthorized { .. }));
}

#[test]
fn reader_write_shaped_op_with_read_retry_kind_is_unauthorized() {
    // Spoof attempt: free-form write-shaped name + MutationKind::Read must not
    // skip mutation auth — operation class is authoritative.
    let identity = RemoteClientIdentity::token(ServiceRole::Reader);
    for name in ["put_evidence", "publish", "enqueue"] {
        let op = RemoteOperation::mutation(StorageCapabilityKind::EvidenceStore, name).expect("op");
        // Honest mutation retry must fail preflight as Unauthorized (reader).
        let envelope = RemoteRequestEnvelope::new(
            identity.clone(),
            op,
            RequestBounds::test_defaults(),
            upsert_retry(&format!("spoof-{name}")),
        )
        .expect("envelope");
        let preflight = envelope.authorize_preflight().expect_err("reader mutate");
        assert!(
            matches!(preflight, StorageError::Unauthorized { .. }),
            "expected Unauthorized for {name}, got {preflight:?}"
        );
    }
}

#[test]
fn envelope_rejects_operation_class_retry_kind_mismatch() {
    // Mutation-class op + Read retry kind → InvalidRequest (fail closed).
    let mut envelope = RemoteRequestEnvelope::new(
        service_identity(),
        mutate_op(),
        RequestBounds::test_defaults(),
        upsert_retry("mismatch-1"),
    )
    .expect("envelope");
    envelope.retry = read_retry();
    let err = envelope.validate().expect_err("class/kind mismatch");
    assert!(matches!(err, StorageError::InvalidRequest { .. }));
    assert!(err.to_string().contains("inconsistent"));

    // Read-class op + Upsert retry kind → InvalidRequest.
    let mut envelope = RemoteRequestEnvelope::new(
        service_identity(),
        read_op(),
        RequestBounds::test_defaults(),
        read_retry(),
    )
    .expect("envelope");
    envelope.retry = upsert_retry("mismatch-2");
    let err = envelope.validate().expect_err("read/mutation mismatch");
    assert!(matches!(err, StorageError::InvalidRequest { .. }));

    // Decode path also fail-closed: mutation class + read kind is invalid.
    // Prefer round-tripping a valid envelope after mutating fields so serde
    // shape stays aligned with the wire types.
    let mut spoof = RemoteRequestEnvelope::new(
        RemoteClientIdentity::token(ServiceRole::Reader),
        RemoteOperation::mutation(StorageCapabilityKind::EvidenceStore, "put_evidence")
            .expect("op"),
        RequestBounds::test_defaults(),
        upsert_retry("decode-spoof"),
    )
    .expect("envelope");
    spoof.retry = read_retry();
    let bytes = serde_json::to_vec(&spoof).expect("json");
    let decoded = decode_remote_request_envelope_json(&bytes).expect_err("spoof decode");
    assert!(matches!(decoded, StorageError::InvalidRequest { .. }));
}

#[test]
fn writer_may_mutate_and_project_storage_auth() {
    let identity = service_identity().with_acl_scope("col-a");
    identity
        .require_mutation("put_evidence")
        .expect("writer ok");
    let auth = identity.to_storage_auth().expect("auth");
    assert_eq!(auth.schema_version, STORAGE_PORTS_SCHEMA_VERSION);
    // Service id preserved; existing acl_scope appended after service: prefix.
    assert_eq!(auth.acl_scope.as_deref(), Some("service:coord-1;col-a"));
    match &auth.principal {
        StoragePrincipal::Token { role } => assert_eq!(role, "editor"),
        other => panic!("expected Token principal, got {other:?}"),
    }
}

#[test]
fn port_principal_projection_maps_roles_and_keeps_service_id() {
    let cases = [
        (ServiceRole::Reader, "reader"),
        (ServiceRole::Writer, "editor"),
        (ServiceRole::Admin, "admin"),
        (ServiceRole::ServicePeer, "admin"),
    ];
    for (remote_role, expected_port_role) in cases {
        let identity = RemoteClientIdentity::service("svc-42", remote_role).expect("identity");
        let auth = identity.to_storage_auth().expect("auth");
        match &auth.principal {
            StoragePrincipal::Token { role } => {
                assert_eq!(
                    role, expected_port_role,
                    "remote role {remote_role:?} should project to {expected_port_role}"
                );
            }
            other => panic!("expected Token, got {other:?}"),
        }
        assert_eq!(auth.acl_scope.as_deref(), Some("service:svc-42"));
    }

    // Token principal has no service_id; acl_scope is only the caller's scope.
    let token = RemoteClientIdentity::token(ServiceRole::Writer).with_acl_scope("tenant-a");
    let auth = token.to_storage_auth().expect("auth");
    match &auth.principal {
        StoragePrincipal::Token { role } => assert_eq!(role, "editor"),
        other => panic!("expected Token, got {other:?}"),
    }
    assert_eq!(auth.acl_scope.as_deref(), Some("tenant-a"));
}

// ---------------------------------------------------------------------------
// Typed remote outcomes
// ---------------------------------------------------------------------------

#[test]
fn typed_timeout_unavailable_conflict_stale_generation() {
    let timeout = RemoteOutcome::timeout("list_sources");
    let err = map_remote_outcome_to_storage_error(&timeout).expect("map");
    assert!(matches!(
        err,
        StorageError::Timeout {
            operation,
            ..
        } if operation == "list_sources"
    ));

    let unavailable = RemoteOutcome::unavailable("partition");
    let err = map_remote_outcome_to_storage_error(&unavailable).expect("map");
    assert!(matches!(err, StorageError::Unavailable { .. }));

    let conflict = RemoteOutcome::conflict("source");
    let err = map_remote_outcome_to_storage_error(&conflict).expect("map");
    assert!(matches!(
        err,
        StorageError::Conflict {
            resource,
            ..
        } if resource == "source"
    ));

    let expected = StorageGeneration::new(3);
    let actual = StorageGeneration::new(2);
    let stale = RemoteOutcome::stale_generation(expected, actual);
    let err = map_remote_outcome_to_storage_error(&stale).expect("map");
    assert!(matches!(
        err,
        StorageError::StaleGeneration {
            expected: e,
            actual: a,
            ..
        } if e.0 == 3 && a.0 == 2
    ));
}

#[test]
fn typed_unauthorized_and_unsupported_map() {
    let unauthorized = RemoteOutcome::unauthorized("no principal");
    let err = map_remote_outcome_to_storage_error(&unauthorized).expect("map");
    assert!(matches!(err, StorageError::Unauthorized { .. }));

    let unsupported = RemoteOutcome::unsupported(StorageCapabilityKind::GraphSearch, "neighbors");
    let err = map_remote_outcome_to_storage_error(&unsupported).expect("map");
    assert!(matches!(
        err,
        StorageError::Unsupported {
            capability: StorageCapabilityKind::GraphSearch,
            operation,
            ..
        } if operation == "neighbors"
    ));
}

#[test]
fn partial_result_is_marked_and_not_mapped_as_error() {
    let meta = PartialResultMeta::new("shard unavailable", true)
        .unwrap()
        .with_resume_cursor("c1")
        .unwrap();
    let outcome = RemoteOutcome::partial(meta);
    assert!(outcome.status.is_partial());
    assert!(outcome.status.is_success());
    let map_err = map_remote_outcome_to_storage_error(&outcome).expect_err("no silent map");
    assert!(matches!(map_err, StorageError::InvalidRequest { .. }));
    assert!(map_err.to_string().contains("PartialResultMeta"));
}

// ---------------------------------------------------------------------------
// Compatibility fail-closed
// ---------------------------------------------------------------------------

#[test]
fn unsupported_schema_and_protocol_fail_closed() {
    let mut offer = CompatibilityOffer::current();
    offer.schema_version = 99;
    let err = offer.validate().expect_err("schema");
    assert!(matches!(err, StorageError::InvalidRequest { .. }));

    let local = CompatibilityOffer::current();
    let mut peer = CompatibilityOffer::current();
    peer.protocol = ProtocolVersion::new(9);
    peer.window = CompatibilityWindow {
        min_protocol: ProtocolVersion::new(9),
        max_protocol: ProtocolVersion::new(9),
        min_schema: SchemaVersion::current(),
        max_schema: SchemaVersion::current(),
    };
    peer.schema_version = REMOTE_STORAGE_CLIENT_SCHEMA_VERSION;
    peer.document_schema = SchemaVersion::current();
    peer.validate().expect("peer self-consistent");
    let err = local.negotiate(&peer).expect_err("no overlap");
    assert!(matches!(err, StorageError::Unsupported { .. }));
    assert!(err.to_string().contains("protocol"));
}

#[test]
fn compatible_peers_negotiate_highest_overlap() {
    let local = CompatibilityOffer::current();
    let peer = CompatibilityOffer::current();
    let negotiated = local.negotiate(&peer).expect("overlap");
    assert_eq!(negotiated.protocol, ProtocolVersion::current());
    assert_eq!(negotiated.document_schema, SchemaVersion::current());
}

#[test]
fn decode_compatibility_offer_rejects_unknown_schema() {
    let json = serde_json::json!({
        "schema_version": 77,
        "protocol": { "major": 1 },
        "document_schema": { "major": 1 },
        "window": {
            "min_protocol": { "major": 1 },
            "max_protocol": { "major": 1 },
            "min_schema": { "major": 1 },
            "max_schema": { "major": 1 }
        }
    });
    let bytes = serde_json::to_vec(&json).unwrap();
    let err = decode_compatibility_offer_json(&bytes).expect_err("unknown");
    assert!(matches!(err, StorageError::InvalidRequest { .. }));
}

// ---------------------------------------------------------------------------
// Idempotent retry classification
// ---------------------------------------------------------------------------

#[test]
fn retry_classification_distinguishes_safe_vs_unsafe() {
    assert_eq!(classify_retry(MutationKind::Read, false), RetryClass::Safe);
    assert_eq!(
        classify_retry(MutationKind::Delete, false),
        RetryClass::Safe
    );
    assert_eq!(
        classify_retry(MutationKind::Upsert, true),
        RetryClass::SafeWithIdempotencyKey
    );
    assert_eq!(
        classify_retry(MutationKind::Upsert, false),
        RetryClass::Unsafe
    );
    assert_eq!(
        classify_retry(MutationKind::Claim, true),
        RetryClass::Unsafe
    );
    assert_eq!(
        classify_retry(MutationKind::Publish, true),
        RetryClass::SafeWithIdempotencyKey
    );

    let safe = RetryPolicy::for_operation(MutationKind::Read, None).unwrap();
    assert!(safe.allows_retry(0));
    assert!(safe.allows_retry(1));
    assert!(!safe.allows_retry(3));

    let unsafe_policy = RetryPolicy::for_operation(MutationKind::Claim, None).unwrap();
    assert_eq!(unsafe_policy.class, RetryClass::Unsafe);
    assert!(!unsafe_policy.allows_retry(0));

    let keyed = upsert_retry("idem-1");
    assert_eq!(keyed.class, RetryClass::SafeWithIdempotencyKey);
    assert!(keyed.allows_retry(0));
}

#[test]
fn upsert_without_idempotency_key_is_unsafe_and_validated() {
    let policy = RetryPolicy::for_operation(MutationKind::Upsert, None).unwrap();
    assert_eq!(policy.class, RetryClass::Unsafe);
    assert_eq!(policy.max_attempts, 1);

    let bad = RetryPolicy {
        kind: MutationKind::Upsert,
        class: RetryClass::SafeWithIdempotencyKey,
        max_attempts: 3,
        idempotency_key: None,
    };
    assert!(bad.validate().is_err());
}

// ---------------------------------------------------------------------------
// Bounds validation
// ---------------------------------------------------------------------------

#[test]
fn bounds_reject_zero_and_absurd_limits() {
    assert!(RequestDeadline::from_timeout(std::time::Duration::from_millis(0)).is_err());
    assert!(PayloadLimits::new(0, 1, 1).is_err());
    assert!(PayloadLimits::new(1, 0, 1).is_err());
    assert!(PayloadLimits::new(1, 1, 0).is_err());
    assert!(PayloadLimits::new(PayloadLimits::ABSURD_MAX_BYTES + 1, 1, 1).is_err());
    assert!(ConcurrencyBound::new(0).is_err());
    assert!(ConcurrencyBound::new(10_001).is_err());
    assert!(QueueBound::new(0).is_err());
    assert!(BoundedPageRequest::new(0).is_err());
    assert!(BoundedPageRequest::new(MAX_PAGE_LIMIT + 1).is_err());
    assert!(RangeReadRequest::new(10, 10).is_err());
    assert!(RangeReadRequest::new(0, MAX_RANGE_BYTES + 1).is_err());
    assert!(StreamChunkHint::new(0).is_err());
    assert!(StreamChunkHint::new(MAX_STREAM_CHUNK_BYTES + 1).is_err());
    assert!(CancellationToken::new("").is_err());
    assert!(IdempotencyKey::new("").is_err());
}

#[test]
fn cancelled_token_surfaces_timeout_class() {
    let token = CancellationToken::new("c1").unwrap().cancel();
    let err = token.check_not_cancelled().expect_err("cancelled");
    assert!(matches!(err, StorageError::Timeout { .. }));
}

// ---------------------------------------------------------------------------
// Envelope + trace + stream shapes
// ---------------------------------------------------------------------------

#[test]
fn request_envelope_authorizes_authenticated_read() {
    let identity = service_identity();
    let trace = RemoteTraceCarrier::from_request_id("req-1", TracePropagationMode::AsyncLink)
        .expect("trace");
    let page = BoundedPageRequest::new(25).unwrap();
    let envelope = RemoteRequestEnvelope::new(
        identity,
        read_op(),
        RequestBounds::test_defaults(),
        read_retry(),
    )
    .unwrap()
    .with_trace(trace)
    .unwrap()
    .with_page(page)
    .unwrap()
    .with_expected_generation(StorageGeneration::new(1));
    envelope.authorize_preflight().expect("authz");
    let outbound = envelope.trace.as_ref().unwrap().outbound_context().unwrap();
    assert_eq!(outbound.request_id, "req-1");
    assert!(outbound.span_id.is_none());
}

#[test]
fn child_span_outbound_opens_real_child() {
    let parent = TraceContext::new(TraceContextFields {
        request_id: "req-child".into(),
        retrieval_run_id: None,
        context_pack_id: None,
        workflow_run_id: None,
        task_id: None,
        publication_generation: None,
        trace_id: Some("trace-aabb".into()),
        span_id: Some("span-parent-01".into()),
        parent_span_id: Some("span-root-00".into()),
    })
    .expect("parent");
    let carrier =
        RemoteTraceCarrier::new(TracePropagationMode::ChildSpan, parent.clone()).expect("carrier");
    let child = carrier.outbound_context().expect("child");
    assert_eq!(child.request_id, parent.request_id);
    assert_eq!(child.trace_id, parent.trace_id);
    assert_ne!(child.span_id, parent.span_id);
    assert!(child.span_id.as_ref().is_some_and(|s| !s.is_empty()));
    assert_eq!(child.parent_span_id, parent.span_id);

    // Second child must also differ from inbound (fresh allocation).
    let child2 = carrier.outbound_context().expect("child2");
    assert_ne!(child2.span_id, parent.span_id);
    assert_ne!(child2.span_id, child.span_id);
    assert_eq!(child2.parent_span_id, parent.span_id);
}

#[test]
fn async_link_and_correlation_only_remain_distinct() {
    let parent = TraceContext::new(TraceContextFields {
        request_id: "req-modes".into(),
        retrieval_run_id: Some("rr-1".into()),
        context_pack_id: None,
        workflow_run_id: None,
        task_id: None,
        publication_generation: None,
        trace_id: Some("trace-cc".into()),
        span_id: Some("span-in".into()),
        parent_span_id: Some("span-grand".into()),
    })
    .expect("parent");

    let async_out = RemoteTraceCarrier::new(TracePropagationMode::AsyncLink, parent.clone())
        .expect("async")
        .outbound_context()
        .expect("out");
    assert_eq!(async_out.request_id, "req-modes");
    assert_eq!(async_out.retrieval_run_id.as_deref(), Some("rr-1"));
    assert_eq!(async_out.trace_id.as_deref(), Some("trace-cc"));
    assert!(async_out.span_id.is_none());
    assert!(async_out.parent_span_id.is_none());

    let corr_out = RemoteTraceCarrier::new(TracePropagationMode::CorrelationOnly, parent)
        .expect("corr")
        .outbound_context()
        .expect("out");
    assert_eq!(corr_out.request_id, "req-modes");
    assert!(corr_out.span_id.is_none());
    assert!(corr_out.parent_span_id.is_none());
    assert!(corr_out.trace_id.is_none());
}

#[test]
fn stream_range_request_validates() {
    let range = RangeReadRequest::new(0, 1024).unwrap();
    assert_eq!(range.len_bytes(), 1024);
    let hint = StreamChunkHint::new(64 * 1024).unwrap();
    let stream = StreamReadRequest::ranged(range, hint).unwrap();
    stream.validate().unwrap();
}

#[test]
fn empty_service_id_rejected() {
    let err = RemoteClientIdentity::service("  ", ServiceRole::ServicePeer).expect_err("blank");
    assert!(matches!(err, StorageError::InvalidRequest { .. }));
}
