//! Authenticated remote storage/index client contract (DIST-005 / issue #351).
//!
//! Walking skeleton: pure client-side semantics layered on [`crate::storage_ports`]
//! — service principal identity, request bounds, idempotency/retry classification,
//! typed remote outcomes (including partial results), protocol/schema negotiation,
//! trace propagation hooks, and streaming/range/pagination request shapes.
//! No HTTP/gRPC transport, TLS, reverse-proxy wiring, or daemon/CLI behavior.
//!
//! Residual: real transport adapters, endpoint exposure for DIST-004 ports,
//! rolling-version matrix, live unauthorized/partition fixtures, closing #351.
//! See `docs/architecture/remote-storage-clients.md`.

mod bounds;
mod compat;
mod idempotency;
mod identity;
mod outcome;
mod request;
mod stream;
mod trace;

pub use bounds::{
    CancellationToken, ConcurrencyBound, PayloadLimits, QueueBound, RequestBounds, RequestDeadline,
};
pub use compat::{
    decode_compatibility_offer_json, CompatibilityOffer, CompatibilityWindow,
    NegotiatedCompatibility, ProtocolVersion, SchemaVersion,
    REMOTE_STORAGE_CLIENT_PROTOCOL_VERSION, REMOTE_STORAGE_CLIENT_SCHEMA_VERSION,
};
pub use idempotency::{classify_retry, IdempotencyKey, MutationKind, RetryClass, RetryPolicy};
pub use identity::{RemoteClientIdentity, RemoteServicePrincipal, ServiceRole};
pub use outcome::{
    map_remote_outcome_to_storage_error, PartialResultMeta, RemoteOutcome, RemoteResult,
    RemoteStatus,
};
pub use request::{
    decode_remote_request_envelope_json, RemoteOperation, RemoteOperationClass,
    RemoteRequestEnvelope,
};
pub use stream::{
    BoundedPageRequest, RangeReadRequest, StreamChunkHint, StreamReadRequest, DEFAULT_PAGE_LIMIT,
    MAX_PAGE_LIMIT, MAX_RANGE_BYTES, MAX_STREAM_CHUNK_BYTES,
};
pub use trace::{RemoteTraceCarrier, TracePropagationMode};

#[cfg(test)]
#[path = "../remote_storage_client_tests.rs"]
mod tests;
