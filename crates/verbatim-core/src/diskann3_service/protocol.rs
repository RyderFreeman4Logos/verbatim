//! Versioned protocol surface declarations. This module does not generate protobuf or start gRPC.

use super::{
    AuthorizationContext, CompletionState, DiskAnn3ServiceDiagnosticCode, DiskAnn3ServiceError,
    DiskAnn3ServiceResult, Generation, IdempotencyKey, PredicatePlan, RequestIdentity,
    SearchRequest, SearchResponse, ServiceCapabilities, ShardHealth, TraceContext,
};

/// Wire schema revision shared by in-process and remote adapters.
pub const DISKANN3_SERVICE_PROTOCOL_VERSION: u32 = 1;

/// Protocol operations available at the service boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolOperation {
    Search,
    RangeSearch,
    ExactRescore,
    Stage,
    Upsert,
    Delete,
    Checkpoint,
    Validate,
    DiscoverCapabilities,
    DiscoverGeneration,
    Health,
    Readiness,
    ShardStatus,
    Cancel,
}

/// Remote wire-equivalent of a search request. It retains every semantic binding.
#[derive(Debug, Clone, PartialEq)]
pub struct ProtocolSearchRequest {
    version: u32,
    identity: RequestIdentity,
    predicate: PredicatePlan,
    authorization: AuthorizationContext,
    trace: TraceContext,
    budget: crate::search_planner::SearchBudget,
    deadline_micros: u64,
    query_vector: Vec<f32>,
    idempotency_key: IdempotencyKey,
}

impl From<&SearchRequest> for ProtocolSearchRequest {
    fn from(request: &SearchRequest) -> Self {
        Self {
            version: DISKANN3_SERVICE_PROTOCOL_VERSION,
            identity: request.identity().clone(),
            predicate: request.predicate().clone(),
            authorization: request.authorization().clone(),
            trace: request.trace().clone(),
            budget: request.budget(),
            deadline_micros: request.deadline_micros(),
            query_vector: request.query_vector().to_vec(),
            idempotency_key: request.idempotency_key().clone(),
        }
    }
}

impl ProtocolSearchRequest {
    pub const fn version(&self) -> u32 {
        self.version
    }
    pub const fn identity(&self) -> &RequestIdentity {
        &self.identity
    }
    pub const fn predicate(&self) -> &PredicatePlan {
        &self.predicate
    }
    pub const fn authorization(&self) -> &AuthorizationContext {
        &self.authorization
    }
    pub const fn trace(&self) -> &TraceContext {
        &self.trace
    }
    pub const fn budget(&self) -> crate::search_planner::SearchBudget {
        self.budget
    }
    pub const fn deadline_micros(&self) -> u64 {
        self.deadline_micros
    }
    pub fn query_vector(&self) -> &[f32] {
        &self.query_vector
    }
    pub const fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }
}

/// Range-search request binds the standard search semantics to a finite radius.
#[derive(Debug, Clone, PartialEq)]
pub struct ProtocolRangeSearchRequest {
    version: u32,
    search_request: ProtocolSearchRequest,
    radius: f32,
}

impl ProtocolRangeSearchRequest {
    pub fn new(request: &SearchRequest, radius: f32) -> DiskAnn3ServiceResult<Self> {
        if !radius.is_finite() || radius < 0.0 {
            return Err(invalid_protocol());
        }
        Ok(Self {
            version: DISKANN3_SERVICE_PROTOCOL_VERSION,
            search_request: request.into(),
            radius,
        })
    }

    pub const fn version(&self) -> u32 {
        self.version
    }
    pub const fn search_request(&self) -> &ProtocolSearchRequest {
        &self.search_request
    }
    pub const fn radius(&self) -> f32 {
        self.radius
    }
}

/// Exact-rescore request binds the standard search semantics to candidate identifiers.
#[derive(Debug, Clone, PartialEq)]
pub struct ProtocolExactRescoreRequest {
    version: u32,
    search_request: ProtocolSearchRequest,
    candidate_ids: Vec<u64>,
}

impl ProtocolExactRescoreRequest {
    pub fn new(request: &SearchRequest, candidate_ids: Vec<u64>) -> DiskAnn3ServiceResult<Self> {
        if !valid_compact_ids(&candidate_ids) {
            return Err(invalid_protocol());
        }
        Ok(Self {
            version: DISKANN3_SERVICE_PROTOCOL_VERSION,
            search_request: request.into(),
            candidate_ids,
        })
    }

    pub const fn version(&self) -> u32 {
        self.version
    }
    pub const fn search_request(&self) -> &ProtocolSearchRequest {
        &self.search_request
    }
    pub fn candidate_ids(&self) -> &[u64] {
        &self.candidate_ids
    }
}

/// Identity-, deadline-, and idempotency-bound staged mutation request.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolMutationRequest {
    version: u32,
    identity: RequestIdentity,
    deadline_micros: u64,
    idempotency_key: IdempotencyKey,
    compact_ids: Vec<u64>,
}

impl ProtocolMutationRequest {
    pub fn new(
        identity: RequestIdentity,
        deadline_micros: u64,
        idempotency_key: IdempotencyKey,
        compact_ids: Vec<u64>,
    ) -> DiskAnn3ServiceResult<Self> {
        if deadline_micros == 0 || !valid_compact_ids(&compact_ids) {
            return Err(invalid_protocol());
        }
        Ok(Self {
            version: DISKANN3_SERVICE_PROTOCOL_VERSION,
            identity,
            deadline_micros,
            idempotency_key,
            compact_ids,
        })
    }

    pub const fn version(&self) -> u32 {
        self.version
    }
    pub const fn identity(&self) -> &RequestIdentity {
        &self.identity
    }
    pub const fn deadline_micros(&self) -> u64 {
        self.deadline_micros
    }
    pub const fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }
    pub fn compact_ids(&self) -> &[u64] {
        &self.compact_ids
    }
}

/// Identity-, deadline-, and idempotency-bound control request for checkpoint or validation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolControlRequest {
    version: u32,
    identity: RequestIdentity,
    deadline_micros: u64,
    idempotency_key: IdempotencyKey,
}

impl ProtocolControlRequest {
    pub fn new(
        identity: RequestIdentity,
        deadline_micros: u64,
        idempotency_key: IdempotencyKey,
    ) -> DiskAnn3ServiceResult<Self> {
        if deadline_micros == 0 {
            return Err(invalid_protocol());
        }
        Ok(Self {
            version: DISKANN3_SERVICE_PROTOCOL_VERSION,
            identity,
            deadline_micros,
            idempotency_key,
        })
    }

    pub const fn version(&self) -> u32 {
        self.version
    }
    pub const fn identity(&self) -> &RequestIdentity {
        &self.identity
    }
    pub const fn deadline_micros(&self) -> u64 {
        self.deadline_micros
    }
    pub const fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }
}

/// Typed checkpoint request.
pub type ProtocolCheckpointRequest = ProtocolControlRequest;
/// Typed validation request.
pub type ProtocolValidateRequest = ProtocolControlRequest;

/// Capability discovery request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolCapabilitiesRequest {
    version: u32,
}

impl Default for ProtocolCapabilitiesRequest {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolCapabilitiesRequest {
    pub const fn new() -> Self {
        Self {
            version: DISKANN3_SERVICE_PROTOCOL_VERSION,
        }
    }
    pub const fn version(&self) -> u32 {
        self.version
    }
}

/// Generation discovery request is bound to an existing identity namespace.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolGenerationDiscoveryRequest {
    version: u32,
    identity: RequestIdentity,
}

impl ProtocolGenerationDiscoveryRequest {
    pub const fn new(identity: RequestIdentity) -> Self {
        Self {
            version: DISKANN3_SERVICE_PROTOCOL_VERSION,
            identity,
        }
    }
    pub const fn version(&self) -> u32 {
        self.version
    }
    pub const fn identity(&self) -> &RequestIdentity {
        &self.identity
    }
}

/// Health or readiness discovery request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolHealthRequest {
    version: u32,
}

impl Default for ProtocolHealthRequest {
    fn default() -> Self {
        Self::new()
    }
}

impl ProtocolHealthRequest {
    pub const fn new() -> Self {
        Self {
            version: DISKANN3_SERVICE_PROTOCOL_VERSION,
        }
    }
    pub const fn version(&self) -> u32 {
        self.version
    }
}

/// Shard-status request carries the immutable identity and a nonzero shard identifier.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolShardStatusRequest {
    version: u32,
    identity: RequestIdentity,
    shard_id: u32,
}

impl ProtocolShardStatusRequest {
    pub fn new(identity: RequestIdentity, shard_id: u32) -> DiskAnn3ServiceResult<Self> {
        if shard_id == 0 {
            return Err(invalid_protocol());
        }
        Ok(Self {
            version: DISKANN3_SERVICE_PROTOCOL_VERSION,
            identity,
            shard_id,
        })
    }
    pub const fn version(&self) -> u32 {
        self.version
    }
    pub const fn identity(&self) -> &RequestIdentity {
        &self.identity
    }
    pub const fn shard_id(&self) -> u32 {
        self.shard_id
    }
}

/// Idempotent cancellation request for one identity-bound operation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolCancelRequest {
    version: u32,
    identity: RequestIdentity,
    idempotency_key: IdempotencyKey,
}

impl ProtocolCancelRequest {
    pub const fn new(identity: RequestIdentity, idempotency_key: IdempotencyKey) -> Self {
        Self {
            version: DISKANN3_SERVICE_PROTOCOL_VERSION,
            identity,
            idempotency_key,
        }
    }
    pub const fn version(&self) -> u32 {
        self.version
    }
    pub const fn identity(&self) -> &RequestIdentity {
        &self.identity
    }
    pub const fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }
}

/// Versioned typed operation envelope for every service request.
#[derive(Debug, Clone, PartialEq)]
pub enum ProtocolOperationRequest {
    Search(ProtocolSearchRequest),
    RangeSearch(ProtocolRangeSearchRequest),
    ExactRescore(ProtocolExactRescoreRequest),
    Stage(ProtocolMutationRequest),
    Upsert(ProtocolMutationRequest),
    Delete(ProtocolMutationRequest),
    Checkpoint(ProtocolCheckpointRequest),
    Validate(ProtocolValidateRequest),
    DiscoverCapabilities(ProtocolCapabilitiesRequest),
    DiscoverGeneration(ProtocolGenerationDiscoveryRequest),
    Health(ProtocolHealthRequest),
    Readiness(ProtocolHealthRequest),
    ShardStatus(ProtocolShardStatusRequest),
    Cancel(ProtocolCancelRequest),
}

impl ProtocolOperationRequest {
    pub const fn operation(&self) -> ProtocolOperation {
        match self {
            Self::Search(_) => ProtocolOperation::Search,
            Self::RangeSearch(_) => ProtocolOperation::RangeSearch,
            Self::ExactRescore(_) => ProtocolOperation::ExactRescore,
            Self::Stage(_) => ProtocolOperation::Stage,
            Self::Upsert(_) => ProtocolOperation::Upsert,
            Self::Delete(_) => ProtocolOperation::Delete,
            Self::Checkpoint(_) => ProtocolOperation::Checkpoint,
            Self::Validate(_) => ProtocolOperation::Validate,
            Self::DiscoverCapabilities(_) => ProtocolOperation::DiscoverCapabilities,
            Self::DiscoverGeneration(_) => ProtocolOperation::DiscoverGeneration,
            Self::Health(_) => ProtocolOperation::Health,
            Self::Readiness(_) => ProtocolOperation::Readiness,
            Self::ShardStatus(_) => ProtocolOperation::ShardStatus,
            Self::Cancel(_) => ProtocolOperation::Cancel,
        }
    }

    pub const fn version(&self) -> u32 {
        match self {
            Self::Search(request) => request.version(),
            Self::RangeSearch(request) => request.version(),
            Self::ExactRescore(request) => request.version(),
            Self::Stage(request) | Self::Upsert(request) | Self::Delete(request) => {
                request.version()
            }
            Self::Checkpoint(request) | Self::Validate(request) => request.version(),
            Self::DiscoverCapabilities(request) => request.version(),
            Self::DiscoverGeneration(request) => request.version(),
            Self::Health(request) | Self::Readiness(request) => request.version(),
            Self::ShardStatus(request) => request.version(),
            Self::Cancel(request) => request.version(),
        }
    }
}

/// Remote response envelope preserves generation, compact results, and completion semantics.
#[derive(Debug, Clone, PartialEq)]
pub struct ProtocolSearchResponse {
    version: u32,
    response: SearchResponse,
}

impl From<&SearchResponse> for ProtocolSearchResponse {
    fn from(response: &SearchResponse) -> Self {
        Self {
            version: DISKANN3_SERVICE_PROTOCOL_VERSION,
            response: response.clone(),
        }
    }
}

impl ProtocolSearchResponse {
    pub const fn version(&self) -> u32 {
        self.version
    }
    pub const fn generation(&self) -> Generation {
        self.response.generation()
    }
    pub const fn completion(&self) -> CompletionState {
        self.response.completion()
    }
    pub const fn response(&self) -> &SearchResponse {
        &self.response
    }
}

/// Capability-discovery response for protocol clients.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolCapabilities {
    version: u32,
    capabilities: ServiceCapabilities,
}

impl ProtocolCapabilities {
    pub const fn new(capabilities: ServiceCapabilities) -> Self {
        Self {
            version: DISKANN3_SERVICE_PROTOCOL_VERSION,
            capabilities,
        }
    }
    pub const fn version(&self) -> u32 {
        self.version
    }
    pub const fn capabilities(&self) -> ServiceCapabilities {
        self.capabilities
    }
}

/// Generation-discovery response preserves its identity namespace and advertised generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolGenerationResponse {
    version: u32,
    identity: RequestIdentity,
}

impl ProtocolGenerationResponse {
    pub const fn new(identity: RequestIdentity) -> Self {
        Self {
            version: DISKANN3_SERVICE_PROTOCOL_VERSION,
            identity,
        }
    }
    pub const fn version(&self) -> u32 {
        self.version
    }
    pub const fn identity(&self) -> &RequestIdentity {
        &self.identity
    }
    pub const fn generation(&self) -> Generation {
        self.identity.generation()
    }
}

/// Health, readiness, and shard-status state reported without a live probe in this slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtocolServingState {
    Healthy,
    Unhealthy,
}

/// Versioned health or readiness response.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolHealthResponse {
    version: u32,
    state: ProtocolServingState,
}

impl ProtocolHealthResponse {
    pub const fn new(state: ProtocolServingState) -> Self {
        Self {
            version: DISKANN3_SERVICE_PROTOCOL_VERSION,
            state,
        }
    }
    pub const fn version(&self) -> u32 {
        self.version
    }
    pub const fn state(&self) -> ProtocolServingState {
        self.state
    }
}

/// Versioned identity-bound shard-status response.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtocolShardStatusResponse {
    version: u32,
    identity: RequestIdentity,
    shard_id: u32,
    health: ShardHealth,
}

impl ProtocolShardStatusResponse {
    pub fn new(
        identity: RequestIdentity,
        shard_id: u32,
        health: ShardHealth,
    ) -> DiskAnn3ServiceResult<Self> {
        if shard_id == 0 {
            return Err(invalid_protocol());
        }
        Ok(Self {
            version: DISKANN3_SERVICE_PROTOCOL_VERSION,
            identity,
            shard_id,
            health,
        })
    }
    pub const fn version(&self) -> u32 {
        self.version
    }
    pub const fn identity(&self) -> &RequestIdentity {
        &self.identity
    }
    pub const fn shard_id(&self) -> u32 {
        self.shard_id
    }
    pub const fn health(&self) -> ShardHealth {
        self.health
    }
}

/// Versioned acknowledgement for mutations, control operations, and cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolAcknowledgement {
    version: u32,
    completion: CompletionState,
}

impl ProtocolAcknowledgement {
    pub const fn new(completion: CompletionState) -> Self {
        Self {
            version: DISKANN3_SERVICE_PROTOCOL_VERSION,
            completion,
        }
    }
    pub const fn version(&self) -> u32 {
        self.version
    }
    pub const fn completion(&self) -> CompletionState {
        self.completion
    }
}

/// Closed protocol failure envelope; the inner error has a stable, redacted code only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProtocolFailure {
    version: u32,
    error: DiskAnn3ServiceError,
}

impl ProtocolFailure {
    pub const fn new(error: DiskAnn3ServiceError) -> Self {
        Self {
            version: DISKANN3_SERVICE_PROTOCOL_VERSION,
            error,
        }
    }
    pub const fn shard_corruption() -> Self {
        Self::new(DiskAnn3ServiceError::contract(
            DiskAnn3ServiceDiagnosticCode::ShardCorruption,
        ))
    }
    pub const fn version(&self) -> u32 {
        self.version
    }
    pub const fn error(&self) -> DiskAnn3ServiceError {
        self.error
    }
}

/// Versioned typed operation envelope for every service response, including failures.
#[derive(Debug, Clone, PartialEq)]
pub enum ProtocolOperationResponse {
    Search(ProtocolSearchResponse),
    RangeSearch(ProtocolSearchResponse),
    ExactRescore(ProtocolSearchResponse),
    Stage(ProtocolAcknowledgement),
    Upsert(ProtocolAcknowledgement),
    Delete(ProtocolAcknowledgement),
    Checkpoint(ProtocolAcknowledgement),
    Validate(ProtocolAcknowledgement),
    DiscoverCapabilities(ProtocolCapabilities),
    DiscoverGeneration(ProtocolGenerationResponse),
    Health(ProtocolHealthResponse),
    Readiness(ProtocolHealthResponse),
    ShardStatus(ProtocolShardStatusResponse),
    Cancel(ProtocolAcknowledgement),
    Failure(ProtocolFailure),
}

impl ProtocolOperationResponse {
    pub const fn shard_corruption() -> Self {
        Self::Failure(ProtocolFailure::shard_corruption())
    }

    /// Returns the operation whose typed response was received; failures are operation-agnostic.
    pub const fn operation(&self) -> Option<ProtocolOperation> {
        match self {
            Self::Search(_) => Some(ProtocolOperation::Search),
            Self::RangeSearch(_) => Some(ProtocolOperation::RangeSearch),
            Self::ExactRescore(_) => Some(ProtocolOperation::ExactRescore),
            Self::Stage(_) => Some(ProtocolOperation::Stage),
            Self::Upsert(_) => Some(ProtocolOperation::Upsert),
            Self::Delete(_) => Some(ProtocolOperation::Delete),
            Self::Checkpoint(_) => Some(ProtocolOperation::Checkpoint),
            Self::Validate(_) => Some(ProtocolOperation::Validate),
            Self::DiscoverCapabilities(_) => Some(ProtocolOperation::DiscoverCapabilities),
            Self::DiscoverGeneration(_) => Some(ProtocolOperation::DiscoverGeneration),
            Self::Health(_) => Some(ProtocolOperation::Health),
            Self::Readiness(_) => Some(ProtocolOperation::Readiness),
            Self::ShardStatus(_) => Some(ProtocolOperation::ShardStatus),
            Self::Cancel(_) => Some(ProtocolOperation::Cancel),
            Self::Failure(_) => None,
        }
    }

    /// Returns the schema revision carried by this typed response envelope.
    pub const fn version(&self) -> u32 {
        match self {
            Self::Search(response) | Self::RangeSearch(response) | Self::ExactRescore(response) => {
                response.version()
            }
            Self::Stage(response)
            | Self::Upsert(response)
            | Self::Delete(response)
            | Self::Checkpoint(response)
            | Self::Validate(response)
            | Self::Cancel(response) => response.version(),
            Self::DiscoverCapabilities(response) => response.version(),
            Self::DiscoverGeneration(response) => response.version(),
            Self::Health(response) | Self::Readiness(response) => response.version(),
            Self::ShardStatus(response) => response.version(),
            Self::Failure(response) => response.version(),
        }
    }

    pub const fn failure(&self) -> Option<DiskAnn3ServiceError> {
        match self {
            Self::Failure(failure) => Some(failure.error()),
            _ => None,
        }
    }
}

fn valid_compact_ids(ids: &[u64]) -> bool {
    !ids.is_empty() && ids.len() <= 4_096 && ids.iter().all(|id| *id != 0)
}

fn invalid_protocol() -> DiskAnn3ServiceError {
    DiskAnn3ServiceError::contract(DiskAnn3ServiceDiagnosticCode::InvalidProtocol)
}
