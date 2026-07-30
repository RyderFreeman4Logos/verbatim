//! Legacy SQLite/HNSW serving cutover contract (MIGRATE-SSD-001 / Refs #388).
//!
//! This types-only walking skeleton models the evidence, migration validation,
//! dual-generation shadow, publication binding, rollback retention, and
//! backup-aware maintenance required before retiring production `low_memory`
//! SQLite full-vector scans and `resident_hnsw` (`instant-distance`) serving.
//! It deliberately contains no DiskANN3 runner, network/daemon wiring, vector
//! re-embedding, live SQLite/HNSW deletion, or config mutation. See
//! `docs/architecture/legacy-vector-cutover.md`.
//!
//! DiskANN3 compilation is explicitly insufficient. Every required gate is
//! represented by a fail-closed type and retirement has no fallback path.

mod error;
mod identity;
mod lifecycle;

pub use error::{LegacyRetirementDiagnosticCode, LegacyRetirementError, LegacyRetirementResult};
pub use identity::{
    CutoverManifest, CutoverManifestFields, PublicationGeneration,
    LEGACY_VECTOR_CUTOVER_SCHEMA_VERSION,
};
pub use lifecycle::{
    authorize_retirement, AuthoritativeVectorSource, CutoverGates, GateClass, LegacyArtifact,
    LegacyArtifactRemovalPlan, LegacyPath, MigrationValidation, MigrationValidationFields,
    PublicationBinding, RemainingServingCapabilities, RetirementAuthorization, RollbackWindow,
    ShadowComparison, ShadowComparisonState, VectorReuseDecision,
};

/// Contract schema version for the legacy-vector-cutover module surface.
pub const LEGACY_VECTOR_CUTOVER_CONTRACT_SCHEMA_VERSION: u32 = LEGACY_VECTOR_CUTOVER_SCHEMA_VERSION;

#[cfg(test)]
#[path = "../legacy_vector_cutover_tests.rs"]
mod tests;
