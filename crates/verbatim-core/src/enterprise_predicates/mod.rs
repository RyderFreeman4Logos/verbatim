//! Enterprise predicate contract for vector search (issue #375).
//!
//! This pure contract module defines how authorization and enterprise metadata
//! constraints (source, tenant, ACL, lifecycle, date ranges, typed metadata)
//! are pushed into vector candidate generation *before or during* traversal —
//! never deferred to unbounded post-filter overfetch.
//!
//! It is deliberately a **walking skeleton**: no live vector search, no DiskANN3
//! binding, no backend integration, no SQLite, no filesystem. It defines typed
//! AST, selectivity classification, generation binding, redaction, and
//! fail-closed errors only. The typed `QueryPlan` remains the public contract;
//! backend JSON syntax must not become the stable API.
//!
//! See `docs/architecture/enterprise-vector-predicates.md`.

mod error;
mod evaluation;
mod generation;
mod hydration;
mod predicate;
#[cfg_attr(not(test), allow(dead_code))]
mod redaction;
mod selectivity;

pub use error::{
    EnterprisePredicateDiagnosticCode, EnterprisePredicateError, EnterprisePredicateResult,
};
pub use evaluation::{
    evaluate_predicates, CandidateGenerationPath, PredicateEvaluation, UnsupportedStrictPredicate,
};
pub use generation::{GenerationBinding, PolicyGeneration, PublicationGenerationBinding};
pub use hydration::{
    CandidateIdentifier, HydrationRevalidation, RevalidationBatch, RevalidationOutcome,
    MAX_REVALIDATION_BATCH,
};
pub use predicate::{
    EnterpriseLifecycleState, EnterprisePredicate, EnterprisePredicateConjunction,
    TypedMetadataValue, MAX_IDENTIFIER_BYTES, MAX_METADATA_KEY_BYTES, MAX_PREDICATES,
};
pub use redaction::{RedactedDebug, RedactionAttested, RedactionReport};
pub use selectivity::{SelectivityClass, SelectivityThresholds, CARDINALITY_REPORT_CEILING};

/// Contract schema version for enterprise predicate documents.
pub const ENTERPRISE_PREDICATES_CONTRACT_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
#[path = "../enterprise_predicates_tests.rs"]
mod tests;
