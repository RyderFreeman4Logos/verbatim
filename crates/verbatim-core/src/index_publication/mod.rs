//! Atomic index publication manifests, promotion, and reconciliation (DIST-006 / issue #352).
//!
//! Walking skeleton: pure contract types for versioned publication manifests,
//! active-generation CAS pointers, completeness/profile/hash validation,
//! stage→validate→promote→rollback state machine, typed reconciliation
//! findings, and query/run binding to a single publication generation.
//! No SQLite/Tantivy/HNSW/Qdrant adapters, daemon wiring, or multi-process
//! coordinator.
//!
//! Residual: real multi-backend promotion, coordinator crash recovery beyond
//! types/docs, GC lease enforcement, daemon/router changes, closing #352.
//! See `docs/architecture/index-publication-manifests.md`.
//!
//! Layering: builds on [`crate::storage_ports::StorageGeneration`] and reuses
//! [`crate::storage_ports::StorageError`] for fail-closed decode/validation.
//! Complements the thinner [`crate::storage_ports::PublicationManifest`] used
//! by the `IndexPublisher` port wire shape.

mod manifest;
mod pointer;
mod promotion;
mod query_binding;
mod reconcile;
mod validate;

pub use manifest::{
    decode_index_publication_manifest_json, BuildStatus, ComponentDigest, ComponentKind,
    DeclaredCapabilities, IndexPublicationManifest, IndexPublicationManifestFields,
    SourceSnapshotRef, INDEX_PUBLICATION_SCHEMA_VERSION,
};
pub use pointer::{decode_active_generation_pointer_json, ActiveGenerationPointer, PointerEpoch};
pub use promotion::{
    InMemoryPublicationCoordinator, PromotionConflict, PromotionOutcome, PromotionPhase,
    PublicationCoordinator,
};
pub use query_binding::{
    decode_query_publication_binding_json, QueryPublicationBinding, QueryPublicationBindingKind,
};
pub use reconcile::{
    ReconciliationFinding, ReconciliationFindingKind, ReconciliationReport, ReconciliationSeverity,
};
pub use validate::{
    validate_completeness, validate_for_promotion, validate_hash_integrity,
    validate_profile_compatibility, validate_referential_integrity, ValidationIssue,
    ValidationReport,
};

#[cfg(test)]
#[path = "../index_publication_tests.rs"]
mod tests;
