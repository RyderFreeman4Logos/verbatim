//! Snapshot-bound opaque cursors and pagination contracts (API-003 / issue #354).
//!
//! Walking skeleton: pure cursor payload + content-hash seal, distinct ranked vs
//! exhaustive page envelopes, typed fail-closed cursor errors, and an in-memory
//! mutation idempotency registry. No store/daemon/HTTP wiring.
//!
//! Residual: live adapter binding, generation retention leases, multi-backend
//! keyset stability, durable idempotency stores, closing #354.
//! See `docs/architecture/snapshot-bound-pagination.md`.
//!
//! Layering: reuses [`crate::storage_ports::PageCursor`] / [`StorageError`] /
//! [`StorageGeneration`] and embeds
//! [`crate::index_publication::QueryPublicationBinding`] for generation fences.

mod cursor;
mod error;
mod idempotency;
mod page;

pub use cursor::{
    decode_cursor_claims, encode_cursor, open_cursor, seal_cursor, validate_cursor_continuation,
    ContinuationContext, CursorClaims, CursorClaimsFields, CursorSealKey, CURSOR_SCHEMA_VERSION,
};
pub use error::{CursorError, CursorResult};
pub use idempotency::{
    IdempotencyClaim, IdempotencyError, InMemoryIdempotencyRegistry, MutationIdempotencyKey,
    MutationOperationFingerprint, MutationResultToken,
};
pub use page::{
    PaginationMode, SnapshotPageRequest, SnapshotPageRequestFields, SnapshotPageResponse,
    DEFAULT_SNAPSHOT_PAGE_LIMIT, MAX_SNAPSHOT_PAGE_LIMIT,
};

#[cfg(test)]
#[path = "../pagination_tests.rs"]
mod tests;
