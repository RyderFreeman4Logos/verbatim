//! Versioned protocol surface declarations. This module does not generate protobuf or start gRPC.

use super::{
    AuthorizationContext, CompletionState, IdempotencyKey, PredicatePlan, RequestIdentity,
    SearchRequest, SearchResponse, ServiceCapabilities, TraceContext,
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
    pub const fn generation(&self) -> super::Generation {
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
