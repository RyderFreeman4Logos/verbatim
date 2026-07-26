//! [`VerbatimClient`] trait — typed public SDK operations.

use async_trait::async_trait;

use super::capability::{CapabilityNegotiation, SdkCapabilityDescriptor, SdkCapabilityKind};
use super::error::ClientResult;
use super::ops::{
    ArtifactGetRequest, ArtifactGetResponse, ContextBuildRequest, ContextBuildResponse,
    EvidenceGetRequest, EvidenceGetResponse, GenerateRequest, GenerateResponse, ResolveRequest,
    ResolveResponse, RetrieveRequest, RetrieveResponse, SearchRequest, SearchResponse,
    SourceUploadRequest, SourceUploadResponse, TaskGetRequest, TaskGetResponse, TaskSubmitRequest,
    TaskSubmitResponse, VerifyRequest, VerifyResponse, WorkflowRunRequest, WorkflowRunResponse,
};

/// Stable public client surface for R/A/G artifact APIs (SDK-001).
///
/// Implementations provide transport (HTTP/gRPC/etc.). This trait is pure
/// contract: no Store, SQL, filesystem, or daemon internals appear in the
/// signatures. Capability negotiation is required before optional features;
/// missing capabilities must surface as typed [`super::ClientError::Unsupported`].
#[async_trait]
pub trait VerbatimClient: Send + Sync {
    /// Discover server capabilities and return a descriptor for caching.
    async fn discover_capabilities(&self) -> ClientResult<SdkCapabilityDescriptor>;

    /// Negotiate required capabilities against an advertised descriptor.
    ///
    /// Default implementation uses [`CapabilityNegotiation`]; overrides may
    /// short-circuit from a warm cache.
    async fn negotiate_capabilities(
        &self,
        required: &[SdkCapabilityKind],
        advertised: &SdkCapabilityDescriptor,
    ) -> ClientResult<SdkCapabilityDescriptor> {
        let negotiation = CapabilityNegotiation::new(required.iter().copied(), advertised.clone())?;
        negotiation.negotiate()
    }

    /// Soft preflight: refuse when a capability is not advertised.
    fn require_capability(
        &self,
        advertised: &SdkCapabilityDescriptor,
        capability: SdkCapabilityKind,
        operation: &str,
    ) -> ClientResult<()> {
        let negotiation = CapabilityNegotiation::new([], advertised.clone())?;
        negotiation.require(capability, operation)
    }

    /// Upload / register a source locator.
    async fn upload_source(
        &self,
        request: SourceUploadRequest,
    ) -> ClientResult<SourceUploadResponse>;

    /// Snapshot-bound search over a QueryPlan identity.
    async fn search(&self, request: SearchRequest) -> ClientResult<SearchResponse>;

    /// Retrieve an EvidencePack for a QueryPlan.
    async fn retrieve(&self, request: RetrieveRequest) -> ClientResult<RetrieveResponse>;

    /// Resolve an artifact reference to a stable locator.
    async fn resolve(&self, request: ResolveRequest) -> ClientResult<ResolveResponse>;

    /// Fetch a previously materialised EvidencePack by content hash.
    async fn get_evidence(&self, request: EvidenceGetRequest) -> ClientResult<EvidenceGetResponse>;

    /// Build a ContextPack from an EvidencePack (A boundary).
    async fn build_context(
        &self,
        request: ContextBuildRequest,
    ) -> ClientResult<ContextBuildResponse>;

    /// Generate a derived artifact from a ContextPack (G boundary).
    async fn generate(&self, request: GenerateRequest) -> ClientResult<GenerateResponse>;

    /// Verify a derived artifact against an evidence pack hash.
    async fn verify(&self, request: VerifyRequest) -> ClientResult<VerifyResponse>;

    /// Start or resume a workflow run.
    async fn run_workflow(&self, request: WorkflowRunRequest) -> ClientResult<WorkflowRunResponse>;

    /// Submit an asynchronous task.
    async fn submit_task(&self, request: TaskSubmitRequest) -> ClientResult<TaskSubmitResponse>;

    /// Poll task status by id.
    async fn get_task(&self, request: TaskGetRequest) -> ClientResult<TaskGetResponse>;

    /// Fetch a public artifact by stable reference.
    async fn get_artifact(&self, request: ArtifactGetRequest) -> ClientResult<ArtifactGetResponse>;
}
