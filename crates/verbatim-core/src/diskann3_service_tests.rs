use crate::diskann3_service::{
    ActiveGenerationSet, AdapterKind, AdmissionContext, AuthorizationContext, BackpressureConfig,
    BackpressureGate, CircuitState, CompletionState, DeltaRecoveryContract,
    DiskAnn3ServiceDiagnosticCode, DiskAnn3ServiceError, Generation, IdempotencyKey,
    ImmutableReplicaSet, InProcessAdapter, PredicatePlan, ProfileId, ProtocolAcknowledgement,
    ProtocolCancelRequest, ProtocolCapabilities, ProtocolCapabilitiesRequest,
    ProtocolCheckpointRequest, ProtocolExactRescoreRequest, ProtocolGenerationDiscoveryRequest,
    ProtocolGenerationResponse, ProtocolHealthRequest, ProtocolHealthResponse,
    ProtocolMutationRequest, ProtocolOperationRequest, ProtocolOperationResponse,
    ProtocolRangeSearchRequest, ProtocolSearchRequest, ProtocolSearchResponse,
    ProtocolServingState, ProtocolShardStatusRequest, ProtocolShardStatusResponse,
    ProtocolValidateRequest, RemoteAdapter, ReplicaEndpoint, RequestIdentity, SearchRequest,
    SearchResponse, ServiceCapabilities, ServiceIdentity, ShardDescriptor, ShardHealth,
    ShardManifest, ShardRouteMetadata, ShardRouter, ShardRouterConfig, TraceContext,
    VectorSearchAdapter, VectorSpaceId, WorkTelemetry,
};
use crate::search_planner::{SearchBudget, SearchBudgetFields};

fn budget() -> SearchBudget {
    SearchBudget::new(SearchBudgetFields {
        result_limit: 5,
        dense_candidate_limit: 10,
        lexical_candidate_limit: 10,
        exact_candidate_limit: 10,
        graph_candidate_limit: 10,
        fused_pool_limit: 10,
        rerank_candidate_limit: 10,
        full_precision_rescore_limit: 5,
        hydration_limit: 5,
        max_ssd_pages: 10,
        max_bytes_read: 1_024,
        max_cpu_micros: 1_024,
        max_work_units: 1_024,
        max_wall_time_micros: 1_024,
        max_concurrent_stages: 1,
        max_stage_attempts: 2,
        debug_record_limit: 5,
    })
    .expect("valid budget")
}

fn identity(generation: u64) -> RequestIdentity {
    RequestIdentity::new(
        ServiceIdentity::new("diskann3-search").expect("service"),
        VectorSpaceId::new("text-default").expect("space"),
        ProfileId::new("default").expect("profile"),
        Generation::new(generation).expect("generation"),
    )
    .expect("identity")
}

fn predicate() -> PredicatePlan {
    PredicatePlan::new(
        "tenant-a",
        vec!["source-a".into()],
        vec!["collection-a".into()],
        vec!["acl-a".into()],
    )
    .expect("predicate")
}

fn request(generation: u64) -> SearchRequest {
    SearchRequest::new(
        identity(generation),
        predicate(),
        AuthorizationContext::attested("tenant-a", vec!["acl-a".into()]).expect("auth"),
        TraceContext::new("trace-0001").expect("trace"),
        budget(),
        1_000,
        vec![1.0; SearchRequest::DIMENSION],
        IdempotencyKey::new("request-0001").expect("idempotency"),
    )
    .expect("request")
}

fn shard(id: u32, generation: u64, health: ShardHealth) -> ShardDescriptor {
    ShardDescriptor::new(
        id,
        identity(generation),
        ShardRouteMetadata::new(
            "tenant-a",
            vec!["source-a".into()],
            vec!["collection-a".into()],
            vec!["acl-a".into()],
        )
        .expect("metadata"),
        health,
        true,
    )
    .expect("shard")
}

#[test]
fn diskann3_service_valid_search_response_is_generation_complete_and_compact() {
    let response = SearchResponse::new(
        vec![(7, 0.25)],
        Generation::new(7).expect("generation"),
        CompletionState::Complete,
        WorkTelemetry::new(1, 2, 3, 4).expect("telemetry"),
    )
    .expect("response");
    assert_eq!(response.generation().value(), 7);
    assert_eq!(response.completion(), CompletionState::Complete);
    assert_eq!(response.results()[0].compact_id(), 7);
    assert_eq!(response.telemetry().ssd_pages(), 1);
    let remote = ProtocolSearchResponse::from(&response);
    assert_eq!(remote.generation(), response.generation());
    assert_eq!(remote.completion(), response.completion());
}

#[test]
fn diskann3_service_router_bounds_fan_out_with_exact_diagnostic() {
    let manifest = ShardManifest::new(
        identity(7),
        vec![
            shard(1, 7, ShardHealth::Ready),
            shard(2, 7, ShardHealth::Ready),
        ],
    )
    .expect("manifest");
    let router = ShardRouter::new(ShardRouterConfig::new(1).expect("config"));
    let error = router
        .route(&request(7), &manifest)
        .expect_err("fan-out must be bounded");
    assert_eq!(
        error.diagnostic_code(),
        DiskAnn3ServiceDiagnosticCode::FanOutExceeded
    );
}

#[test]
fn diskann3_service_auth_uncertainty_fails_closed_not_partial() {
    let manifest = ShardManifest::new(identity(7), vec![shard(1, 7, ShardHealth::Unavailable)])
        .expect("manifest");
    let router = ShardRouter::new(ShardRouterConfig::new(2).expect("config"));
    let request = request(7).with_authorization(AuthorizationContext::uncertain());
    let error = router
        .route(&request, &manifest)
        .expect_err("unknown ACL must fail closed");
    assert_eq!(
        error.diagnostic_code(),
        DiskAnn3ServiceDiagnosticCode::AuthorizationUncertain
    );
}

#[test]
fn diskann3_service_required_unavailable_shard_is_explicit_partial() {
    let manifest = ShardManifest::new(identity(7), vec![shard(1, 7, ShardHealth::Unavailable)])
        .expect("manifest");
    let router = ShardRouter::new(ShardRouterConfig::new(2).expect("config"));
    let route = router
        .route(&request(7), &manifest)
        .expect("typed partial route");
    assert!(route.is_partial());
    assert_eq!(route.unavailable_shards(), &[1]);
}

#[test]
fn diskann3_service_rejects_incompatible_dual_active_generations() {
    let first = ImmutableReplicaSet::new(
        identity(7),
        vec![ReplicaEndpoint::new("node-a").expect("node")],
    )
    .expect("replicas");
    let second = ImmutableReplicaSet::new(
        identity(8),
        vec![ReplicaEndpoint::new("node-b").expect("node")],
    )
    .expect("replicas");
    let error =
        ActiveGenerationSet::new(vec![first, second]).expect_err("only one active generation");
    assert_eq!(
        error.diagnostic_code(),
        DiskAnn3ServiceDiagnosticCode::IncompatibleActiveGeneration
    );
}

#[test]
fn diskann3_service_rejects_stale_generation_with_exact_code() {
    let manifest =
        ShardManifest::new(identity(8), vec![shard(1, 8, ShardHealth::Ready)]).expect("manifest");
    let router = ShardRouter::new(ShardRouterConfig::new(2).expect("config"));
    let error = router
        .route(&request(7), &manifest)
        .expect_err("stale generation");
    assert_eq!(
        error.diagnostic_code(),
        DiskAnn3ServiceDiagnosticCode::StaleGeneration
    );
}

#[test]
fn diskann3_service_rejects_wrong_service_identity_with_exact_code() {
    let other_identity = RequestIdentity::new(
        ServiceIdentity::new("other-service").expect("service"),
        VectorSpaceId::new("text-default").expect("space"),
        ProfileId::new("default").expect("profile"),
        Generation::new(7).expect("generation"),
    )
    .expect("identity");
    let manifest = ShardManifest::new(
        other_identity.clone(),
        vec![ShardDescriptor::new(
            1,
            other_identity,
            ShardRouteMetadata::new(
                "tenant-a",
                vec!["source-a".into()],
                vec!["collection-a".into()],
                vec!["acl-a".into()],
            )
            .expect("metadata"),
            ShardHealth::Ready,
            true,
        )
        .expect("shard")],
    )
    .expect("manifest");
    let error = ShardRouter::new(ShardRouterConfig::new(2).expect("config"))
        .route(&request(7), &manifest)
        .expect_err("service identity must not cross the routing boundary");
    assert_eq!(
        error.diagnostic_code(),
        DiskAnn3ServiceDiagnosticCode::GenerationMismatch
    );
}

#[test]
fn diskann3_service_deadline_and_budget_are_bound_and_exhaustion_is_typed() {
    let error = SearchRequest::new(
        identity(7),
        predicate(),
        AuthorizationContext::attested("tenant-a", vec!["acl-a".into()]).expect("auth"),
        TraceContext::new("trace-0002").expect("trace"),
        budget(),
        1_025,
        vec![1.0; SearchRequest::DIMENSION],
        IdempotencyKey::new("request-0002").expect("key"),
    )
    .expect_err("deadline cannot exceed the shared budget");
    assert_eq!(
        error.diagnostic_code(),
        DiskAnn3ServiceDiagnosticCode::DeadlineExceeded
    );
}

#[test]
fn diskann3_service_retry_cannot_reset_or_widen_budget() {
    let error = BackpressureConfig::new(1, 1, 10, budget(), budget(), 2)
        .expect_err("a retry must consume a narrower remaining budget");
    assert_eq!(
        error.diagnostic_code(),
        DiskAnn3ServiceDiagnosticCode::RetryBudgetReset
    );
}

#[test]
fn diskann3_service_in_process_and_remote_share_one_semantic_surface() {
    let request = request(7);
    let local = InProcessAdapter::new();
    let remote = RemoteAdapter::new();
    assert_eq!(local.kind(), AdapterKind::InProcess);
    assert_eq!(remote.kind(), AdapterKind::Remote);
    assert_eq!(
        local.semantic_identity(&request),
        remote.semantic_identity(&request)
    );
    let protocol = ProtocolSearchRequest::from(&request);
    assert_eq!(protocol.identity(), request.identity());
    assert_eq!(protocol.predicate(), request.predicate());
    assert_eq!(protocol.authorization(), request.authorization());
    assert_eq!(protocol.trace(), request.trace());
    assert_eq!(protocol.budget(), request.budget());
    assert_eq!(protocol.deadline_micros(), request.deadline_micros());

    let local_response = local.search(&request).expect("local typed search outcome");
    let remote_response = remote
        .search(&request)
        .expect("remote typed search outcome");
    assert_eq!(local_response, remote_response);
    assert_eq!(local_response.generation(), request.identity().generation());
    assert_eq!(local_response.completion(), CompletionState::Complete);
    assert!(local_response.telemetry().work_units() <= request.budget().fields().max_work_units);

    let range_request = ProtocolRangeSearchRequest::new(&request, 0.5).expect("range request");
    assert_eq!(
        local.range_search(&range_request).expect("local range"),
        remote.range_search(&range_request).expect("remote range"),
    );
    let rescore_request =
        ProtocolExactRescoreRequest::new(&request, vec![7]).expect("rescore request");
    assert_eq!(
        local
            .exact_rescore(&rescore_request)
            .expect("local rescore"),
        remote
            .exact_rescore(&rescore_request)
            .expect("remote rescore"),
    );
}

#[test]
fn diskann3_service_durable_replica_serde_revalidates_and_rejects_network_storage() {
    let encoded = r#"{"identity":{"service":"diskann3-search","vector_space":"text-default","profile":"default","generation":7},"replicas":["node-a"],"storage":"shared_mutable_nfs"}"#;
    assert!(serde_json::from_str::<ImmutableReplicaSet>(encoded).is_err());
}

#[test]
fn diskann3_service_circuit_open_and_queue_overload_have_exact_codes() {
    let gate = BackpressureGate::new(
        BackpressureConfig::new(1, 1, 10, budget(), narrower_budget(), 2).expect("backpressure"),
        CircuitState::Open,
    );
    let error = gate
        .admit(admission(0, 0, 0, 0, 0))
        .expect_err("open circuit");
    assert_eq!(
        error.diagnostic_code(),
        DiskAnn3ServiceDiagnosticCode::CircuitOpen
    );

    let gate = BackpressureGate::new(
        BackpressureConfig::new(1, 1, 10, budget(), narrower_budget(), 2).expect("backpressure"),
        CircuitState::Closed,
    );
    let error = gate
        .admit(admission(0, 1, 0, 0, 0))
        .expect_err("bounded queue");
    assert_eq!(
        error.diagnostic_code(),
        DiskAnn3ServiceDiagnosticCode::QueueExceeded
    );
}

#[test]
fn diskann3_service_backpressure_rejects_tenant_work_and_expired_queue_deadline() {
    let gate = BackpressureGate::new(
        BackpressureConfig::new(2, 2, 10, budget(), narrower_budget(), 10).expect("backpressure"),
        CircuitState::Closed,
    );
    let error = gate
        .admit(admission(0, 0, 8, 3, 0))
        .expect_err("charged tenant work exceeds the configured cap");
    assert_eq!(
        error.diagnostic_code(),
        DiskAnn3ServiceDiagnosticCode::TenantWorkExceeded
    );

    let error = gate
        .admit(admission(0, 0, 0, 0, 10))
        .expect_err("queue wait at its deadline is expired");
    assert_eq!(
        error.diagnostic_code(),
        DiskAnn3ServiceDiagnosticCode::QueueDeadlineExceeded
    );
}

#[test]
fn diskann3_service_non_search_protocol_envelopes_are_versioned_and_typed() {
    let request = request(7);
    let mutation = ProtocolMutationRequest::new(
        request.identity().clone(),
        request.deadline_micros(),
        request.idempotency_key().clone(),
        vec![7],
    )
    .expect("mutation request");
    let envelopes = vec![
        ProtocolOperationRequest::RangeSearch(
            ProtocolRangeSearchRequest::new(&request, 0.5).expect("range request"),
        ),
        ProtocolOperationRequest::ExactRescore(
            ProtocolExactRescoreRequest::new(&request, vec![7]).expect("rescore request"),
        ),
        ProtocolOperationRequest::Stage(mutation.clone()),
        ProtocolOperationRequest::Upsert(mutation.clone()),
        ProtocolOperationRequest::Delete(mutation),
        ProtocolOperationRequest::Checkpoint(
            ProtocolCheckpointRequest::new(
                request.identity().clone(),
                request.deadline_micros(),
                request.idempotency_key().clone(),
            )
            .expect("checkpoint request"),
        ),
        ProtocolOperationRequest::Validate(
            ProtocolValidateRequest::new(
                request.identity().clone(),
                request.deadline_micros(),
                request.idempotency_key().clone(),
            )
            .expect("validate request"),
        ),
        ProtocolOperationRequest::DiscoverCapabilities(ProtocolCapabilitiesRequest::new()),
        ProtocolOperationRequest::DiscoverGeneration(ProtocolGenerationDiscoveryRequest::new(
            request.identity().clone(),
        )),
        ProtocolOperationRequest::Health(ProtocolHealthRequest::new()),
        ProtocolOperationRequest::Readiness(ProtocolHealthRequest::new()),
        ProtocolOperationRequest::ShardStatus(
            ProtocolShardStatusRequest::new(request.identity().clone(), 1)
                .expect("shard-status request"),
        ),
        ProtocolOperationRequest::Cancel(ProtocolCancelRequest::new(
            request.identity().clone(),
            request.idempotency_key().clone(),
        )),
    ];
    assert!(envelopes.iter().all(|envelope| envelope.version() == 1));
}

#[test]
fn diskann3_service_non_search_protocol_responses_are_versioned_and_typed() {
    let request = request(7);
    let search_response = ProtocolSearchResponse::from(
        &SearchResponse::new(
            vec![(7, 0.25)],
            request.identity().generation(),
            CompletionState::Complete,
            WorkTelemetry::new(1, 1, 1, 1).expect("telemetry"),
        )
        .expect("search response"),
    );
    let acknowledgement = ProtocolAcknowledgement::new(CompletionState::Complete);
    let capabilities = ProtocolCapabilities::new(
        ServiceCapabilities::new(true, true, true, true, true).expect("capabilities"),
    );
    let health = ProtocolHealthResponse::new(ProtocolServingState::Healthy);
    let shard_status =
        ProtocolShardStatusResponse::new(request.identity().clone(), 1, ShardHealth::Ready)
            .expect("shard status");
    let responses = vec![
        ProtocolOperationResponse::RangeSearch(search_response.clone()),
        ProtocolOperationResponse::ExactRescore(search_response),
        ProtocolOperationResponse::Stage(acknowledgement),
        ProtocolOperationResponse::Upsert(acknowledgement),
        ProtocolOperationResponse::Delete(acknowledgement),
        ProtocolOperationResponse::Checkpoint(acknowledgement),
        ProtocolOperationResponse::Validate(acknowledgement),
        ProtocolOperationResponse::DiscoverCapabilities(capabilities),
        ProtocolOperationResponse::DiscoverGeneration(ProtocolGenerationResponse::new(
            request.identity().clone(),
        )),
        ProtocolOperationResponse::Health(health),
        ProtocolOperationResponse::Readiness(health),
        ProtocolOperationResponse::ShardStatus(shard_status),
        ProtocolOperationResponse::Cancel(acknowledgement),
    ];
    assert!(responses.iter().all(|response| response.version() == 1));
    assert!(responses
        .iter()
        .all(|response| response.operation().is_some()));
}

#[test]
fn diskann3_service_protocol_corruption_diagnostic_has_exact_code() {
    let response = ProtocolOperationResponse::shard_corruption();
    let error = response.failure().expect("typed corruption failure");
    assert_eq!(
        error.diagnostic_code(),
        DiskAnn3ServiceDiagnosticCode::ShardCorruption
    );
    assert_eq!(error.to_string(), "diskann3-service.shard_corruption");
}

#[test]
fn diskann3_service_capability_discovery_rejects_missing_remote_semantics() {
    let error = ServiceCapabilities::new(false, true, true, true, true)
        .expect_err("remote contract must preserve all semantic fields");
    assert_eq!(
        error.diagnostic_code(),
        DiskAnn3ServiceDiagnosticCode::UnsupportedCapability
    );
}

#[test]
fn diskann3_service_update_durability_is_explicit_not_ann_claimed() {
    let error =
        DeltaRecoveryContract::ann_library_only().expect_err("ANN durability is insufficient");
    assert_eq!(
        error.diagnostic_code(),
        DiskAnn3ServiceDiagnosticCode::DurabilityContractRequired
    );
}

#[test]
fn diskann3_service_diagnostics_are_code_only_and_redacted() {
    let error =
        DiskAnn3ServiceError::contract(DiskAnn3ServiceDiagnosticCode::AuthorizationUncertain);
    assert_eq!(
        error.to_string(),
        "diskann3-service.authorization_uncertain"
    );
    assert_eq!(
        format!("{error:?}"),
        "DiskAnn3ServiceError(authorization_uncertain)"
    );
}

fn narrower_budget() -> SearchBudget {
    let mut fields = budget().fields();
    fields.max_stage_attempts = 1;
    SearchBudget::new(fields).expect("narrower budget")
}

fn admission(
    active_queries: u32,
    queued_queries: u32,
    observed_tenant_work: u64,
    charged_tenant_work: u64,
    queue_wait_micros: u64,
) -> AdmissionContext<'static> {
    AdmissionContext::new(
        "tenant-a",
        active_queries,
        queued_queries,
        observed_tenant_work,
        charged_tenant_work,
        queue_wait_micros,
    )
    .expect("admission context")
}
