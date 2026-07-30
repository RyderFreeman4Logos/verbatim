//! Sealed in-process and remote adapters sharing one semantic `VectorSearch` surface.

use super::{
    CompletionState, DiskAnn3ServiceResult, ProtocolExactRescoreRequest,
    ProtocolRangeSearchRequest, RequestIdentity, SearchRequest, SearchResponse, WorkTelemetry,
};

mod sealed {
    /// Private marker prevents unchecked downstream adapter implementations.
    pub trait Sealed {}
}

/// Dispatch representation; neither variant claims live transport behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdapterKind {
    InProcess,
    Remote,
}

/// Shared conformance surface for both local and remote request paths.
///
/// Each operation consumes the same validated semantic requests and returns a typed
/// response bound to their generation, completion, and bounded-work semantics. The
/// placeholder outcome is deliberately I/O-free; a runtime implementation replaces it
/// without changing this trait.
pub trait VectorSearchAdapter: sealed::Sealed {
    fn kind(&self) -> AdapterKind;
    fn semantic_identity(&self, request: &SearchRequest) -> RequestIdentity;
    fn search(&self, request: &SearchRequest) -> DiskAnn3ServiceResult<SearchResponse>;
    fn range_search(
        &self,
        request: &ProtocolRangeSearchRequest,
    ) -> DiskAnn3ServiceResult<SearchResponse>;
    fn exact_rescore(
        &self,
        request: &ProtocolExactRescoreRequest,
    ) -> DiskAnn3ServiceResult<SearchResponse>;
}

/// Local all-in-one semantic adapter marker.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct InProcessAdapter;

impl InProcessAdapter {
    pub const fn new() -> Self {
        Self
    }
}

impl sealed::Sealed for InProcessAdapter {}

impl VectorSearchAdapter for InProcessAdapter {
    fn kind(&self) -> AdapterKind {
        AdapterKind::InProcess
    }
    fn semantic_identity(&self, request: &SearchRequest) -> RequestIdentity {
        request.identity().clone()
    }
    fn search(&self, request: &SearchRequest) -> DiskAnn3ServiceResult<SearchResponse> {
        skeleton_response(request.identity())
    }
    fn range_search(
        &self,
        request: &ProtocolRangeSearchRequest,
    ) -> DiskAnn3ServiceResult<SearchResponse> {
        skeleton_response(request.search_request().identity())
    }
    fn exact_rescore(
        &self,
        request: &ProtocolExactRescoreRequest,
    ) -> DiskAnn3ServiceResult<SearchResponse> {
        skeleton_response(request.search_request().identity())
    }
}

/// Shared-nothing remote semantic adapter marker; no network client is created here.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct RemoteAdapter;

impl RemoteAdapter {
    pub const fn new() -> Self {
        Self
    }
}

impl sealed::Sealed for RemoteAdapter {}

impl VectorSearchAdapter for RemoteAdapter {
    fn kind(&self) -> AdapterKind {
        AdapterKind::Remote
    }
    fn semantic_identity(&self, request: &SearchRequest) -> RequestIdentity {
        request.identity().clone()
    }
    fn search(&self, request: &SearchRequest) -> DiskAnn3ServiceResult<SearchResponse> {
        skeleton_response(request.identity())
    }
    fn range_search(
        &self,
        request: &ProtocolRangeSearchRequest,
    ) -> DiskAnn3ServiceResult<SearchResponse> {
        skeleton_response(request.search_request().identity())
    }
    fn exact_rescore(
        &self,
        request: &ProtocolExactRescoreRequest,
    ) -> DiskAnn3ServiceResult<SearchResponse> {
        skeleton_response(request.search_request().identity())
    }
}

fn skeleton_response(identity: &RequestIdentity) -> DiskAnn3ServiceResult<SearchResponse> {
    SearchResponse::new(
        Vec::new(),
        identity.generation(),
        CompletionState::Complete,
        WorkTelemetry::new(1, 1, 1, 1)?,
    )
}
