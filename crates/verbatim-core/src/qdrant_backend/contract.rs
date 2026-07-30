//! Sealed walking-skeleton adapter contract for the Qdrant enterprise reference.

use async_trait::async_trait;

use super::{
    GrpcPathRequirements, HydrationRequest, PayloadIndexPlan, QdrantBackendResult,
    QdrantCapabilities, QdrantCollectionSchema, QdrantFilterContract, QdrantLexicalPolicy,
    QdrantOperationBudget, QdrantSearchOutcome, QdrantSearchPolicy,
};

mod sealed {
    /// Marker implemented only by crate-owned Qdrant adapter types.
    pub trait Sealed {}
}

/// Mutation hook envelope (types-only; no live network).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct QdrantMutationHook {
    pub upsert_allowed: bool,
    pub delete_allowed: bool,
    pub requires_idempotency: bool,
}

impl QdrantMutationHook {
    pub fn new(
        upsert_allowed: bool,
        delete_allowed: bool,
        requires_idempotency: bool,
    ) -> QdrantBackendResult<Self> {
        if (upsert_allowed || delete_allowed) && !requires_idempotency {
            return Err(super::QdrantBackendError::contract(
                super::QdrantBackendDiagnosticCode::InvalidMutationHook,
            ));
        }
        Ok(Self {
            upsert_allowed,
            delete_allowed,
            requires_idempotency,
        })
    }
}

/// Validated search request surface for the contract walking skeleton.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct QdrantSearchRequest {
    schema: QdrantCollectionSchema,
    policy: QdrantSearchPolicy,
    filter: QdrantFilterContract,
    budget: QdrantOperationBudget,
    limit: u32,
}

impl QdrantSearchRequest {
    pub fn new(
        schema: QdrantCollectionSchema,
        policy: QdrantSearchPolicy,
        filter: QdrantFilterContract,
        budget: QdrantOperationBudget,
        limit: u32,
    ) -> QdrantBackendResult<Self> {
        if limit == 0 || limit > budget.operation_budget().fields().result_limit {
            return Err(super::QdrantBackendError::contract(
                super::QdrantBackendDiagnosticCode::InvalidSearchPolicy,
            ));
        }
        if !policy.is_qdrant_primary() {
            return Err(super::QdrantBackendError::contract(
                super::QdrantBackendDiagnosticCode::UnconditionalLocalPreSearchForbidden,
            ));
        }
        Ok(Self {
            schema,
            policy,
            filter,
            budget,
            limit,
        })
    }

    pub const fn schema(&self) -> &QdrantCollectionSchema {
        &self.schema
    }

    pub const fn policy(&self) -> &QdrantSearchPolicy {
        &self.policy
    }

    pub const fn filter(&self) -> &QdrantFilterContract {
        &self.filter
    }

    pub const fn budget(&self) -> &QdrantOperationBudget {
        &self.budget
    }

    pub const fn limit(&self) -> u32 {
        self.limit
    }
}

/// Crate-owned walking-skeleton adapter contract for Qdrant reference backends.
///
/// This trait is deliberately sealed: its marker remains private to this module, so
/// downstream crates cannot implement the adapter contract. It does not require live
/// network I/O. Transitional REST lives in `crate::index::qdrant` until a gRPC cutover lands.
#[async_trait]
pub trait QdrantVectorSearch: sealed::Sealed {
    /// Validate collection schema, named vectors, and payload-index prerequisites.
    async fn validate_schema(
        &self,
        schema: QdrantCollectionSchema,
        payload_indexes: PayloadIndexPlan,
    ) -> QdrantBackendResult<()>;

    /// Execute a Qdrant-primary, budget-bound search request and return its outcome.
    async fn search(
        &self,
        request: QdrantSearchRequest,
    ) -> QdrantBackendResult<QdrantSearchOutcome>;

    /// Hydrate authoritative evidence and reject stale/wrong-generation points.
    async fn hydrate(&self, request: HydrationRequest) -> QdrantBackendResult<()>;

    /// Discover Query API / named-vector / quantization / on-disk capabilities.
    fn capabilities(&self) -> QdrantBackendResult<QdrantCapabilities>;

    /// Report lexical ownership (Tantivy primary unless #380 conformance passes).
    fn lexical_policy(&self) -> QdrantBackendResult<QdrantLexicalPolicy>;

    /// Report gRPC / official-client path requirements (types-only).
    fn grpc_path_requirements(&self) -> QdrantBackendResult<GrpcPathRequirements>;

    /// Mutation hooks remain generation-bound and idempotent when enabled.
    fn mutation_hooks(&self) -> QdrantBackendResult<QdrantMutationHook>;
}
