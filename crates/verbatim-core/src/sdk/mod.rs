//! Stable public SDK client trait contracts (SDK-001 / issue #355).
//!
//! Walking skeleton: pure client configuration, capability negotiation, typed
//! errors, operation request/response envelopes, a [`VerbatimClient`] trait
//! surface for R/A/G artifact APIs, and a cursor iterator layered on
//! [`crate::pagination`]. No HTTP/transport implementation, no daemon/CLI
//! wiring, and no local Store/SQL/filesystem exposure.
//!
//! Residual: real transport adapters, SSE/progress streams, retries, OpenAPI
//! generated clients, examples, compatibility matrix, closing #355.
//! See `docs/architecture/stable-sdk-contracts.md`.

mod capability;
mod client;
mod config;
mod cursor_iter;
mod error;
mod ops;
mod workflow_run;

pub use capability::{
    decode_sdk_capability_descriptor_json, CapabilityCache, CapabilityNegotiation,
    SdkCapabilityDescriptor, SdkCapabilityKind, SDK_CAPABILITY_SCHEMA_VERSION,
};
pub use client::VerbatimClient;
pub use config::{SdkConfig, SdkConfigFields, DEFAULT_SDK_TIMEOUT_SECS, DEFAULT_SDK_USER_AGENT};
pub use cursor_iter::{CursorIterator, CursorPageFetcher};
pub use error::{ClientError, ClientResult};
pub use ops::{
    ArtifactGetRequest, ArtifactGetResponse, ArtifactRef, ContextBuildRequest,
    ContextBuildResponse, EvidenceGetRequest, EvidenceGetResponse, GenerateRequest,
    GenerateResponse, ResolveRequest, ResolveResponse, RetrieveRequest, RetrieveResponse,
    SearchRequest, SearchResponse, SearchResultItem, SourceUploadRequest, SourceUploadResponse,
    TaskGetRequest, TaskGetResponse, TaskSubmitRequest, TaskSubmitResponse, VerifyRequest,
    VerifyResponse, WorkflowRunRequest, WorkflowRunResponse,
};

#[cfg(test)]
#[path = "../sdk_tests.rs"]
mod tests;
