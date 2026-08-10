//! Retrieval-pipeline telemetry contract (issue #387).
//!
//! This pure walking skeleton defines bounded stage spans, candidate/storage/
//! resource counters, cgroup-memory snapshots, backend attributes, and a
//! fail-closed privacy boundary. It has no live tracing exporter, metric backend,
//! cgroup reader, SSD sampler, DiskANN3 binding, daemon, or CLI wiring.
//!
//! See `docs/architecture/retrieval-telemetry.md`.

mod attribute;
mod counter;
mod error;
mod memory;
mod privacy;
mod span;

pub use attribute::{
    BackendAttribute, BackendAttributeKey, BackendAttributeValue, DiskannProviderLayout,
    LanceDbIndexType, QdrantQuantization, MAX_BACKEND_ATTRIBUTE_NUMERIC_VALUE,
};
pub use counter::{
    CandidateCounters, ResourceCounters, RetrievalResourceCounters, StageCandidateCounters,
    StorageAccessMode, StorageCounters,
};
pub use error::{TelemetryDiagnosticCode, TelemetryError, TelemetryResult};
pub use memory::{
    MemoryEventCounters, MemorySnapshot, MemorySnapshotFields, MAX_MEMORY_SNAPSHOT_BYTES,
};
pub use privacy::{
    PrivacyPolicy, RedactedTelemetryId, TelemetryDataClass, TelemetryDestination,
    MAX_REDACTED_TELEMETRY_ID_SOURCE_BYTES,
};
pub use span::{SpanKind, StageSpan, StageSpanFields, MAX_STAGE_DURATION_MICROS};

/// Contract schema version for retrieval-telemetry documents.
pub const RETRIEVAL_TELEMETRY_CONTRACT_SCHEMA_VERSION: u32 = 1;
