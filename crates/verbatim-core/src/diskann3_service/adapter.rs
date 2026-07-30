//! Sealed in-process and remote adapters sharing one semantic `VectorSearch` surface.

use super::{RequestIdentity, SearchRequest};

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
/// Implementations must preserve identity, predicates, budget, deadline, idempotency,
/// and completion semantics. Live I/O is deliberately outside this walking skeleton.
pub trait VectorSearchAdapter: sealed::Sealed {
    fn kind(&self) -> AdapterKind;
    fn semantic_identity(&self, request: &SearchRequest) -> RequestIdentity;
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
}
