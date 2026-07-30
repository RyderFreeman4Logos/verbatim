//! Sealed VectorSearch-style contract for the types-only LanceDB reference adapter.

use async_trait::async_trait;

use crate::storage_ports::VectorSearch;

use super::{
    CandidateLossReport, LanceDbBackendResult, LanceDbCapabilities, LanceDbCollectionSchema,
    LanceDbHitRef, LanceDbIndexProfile, LanceDbLexicalPolicy, LanceDbLifecycleTransition,
    LanceDbScalarIndexPlan, LanceDbSearchRequest,
};

mod sealed {
    /// Marker implemented only by crate-owned LanceDB adapter types.
    pub trait Sealed {}
}

/// Crate-owned reference adapter surface, with no live LanceDB dependency or I/O in this slice.
#[async_trait]
pub trait LanceDbVectorSearch: sealed::Sealed + VectorSearch {
    /// Validate the table schema, index profile, and scalar-index prerequisites before use.
    async fn validate_schema(
        &self,
        schema: LanceDbCollectionSchema,
        profile: LanceDbIndexProfile,
        scalar_indexes: LanceDbScalarIndexPlan,
    ) -> LanceDbBackendResult<()>;

    /// Execute a LanceDB-primary, generation-bound candidate generation request.
    async fn search(
        &self,
        request: LanceDbSearchRequest,
    ) -> LanceDbBackendResult<CandidateLossReport>;

    /// Hydrate an authoritative candidate only after exact profile/generation validation.
    async fn hydrate(&self, hit: LanceDbHitRef) -> LanceDbBackendResult<()>;

    /// Apply one staged generation lifecycle transition (including optimization/reindex hooks).
    async fn transition_lifecycle(
        &self,
        transition: LanceDbLifecycleTransition,
    ) -> LanceDbBackendResult<()>;

    /// Discover required IVF, scalar prefilter, refine, and publication capabilities.
    fn capabilities(&self) -> LanceDbBackendResult<LanceDbCapabilities>;

    /// Report the Tantivy-primary / LanceDB-FTS comparison-only ownership policy.
    fn lexical_policy(&self) -> LanceDbBackendResult<LanceDbLexicalPolicy>;
}
