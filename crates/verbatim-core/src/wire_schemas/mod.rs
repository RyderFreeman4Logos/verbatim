//! Versioned R/A/G wire schema contracts (API-002 / issue #353).
//!
//! Walking skeleton: shared schema version, canonical identity / content-hash
//! hooks, and minimal typed envelopes for QueryPlan, EvidencePack, ContextPack,
//! DerivedArtifact, and WorkflowEnvelope. Decode fails closed on unknown
//! schema versions, fields, and invalid identity/hash fields. Namespaced header
//! extensions provide optional data without changing body content identity.
//! Byte-stable JSON helpers support golden round-trips.
//!
//! Residual: full production field sets, multi-version dual-shape or permissive
//! decode, OpenAPI/JSON Schema generators, daemon/CLI/SDK adoption, dual
//! audit/redacted views, closing #353. See
//! `docs/architecture/versioned-wire-schemas.md`.

mod common;
mod derived;
mod envelopes;
mod identity;
mod ser;

pub use common::{
    validate_wire_schema_version, WireArtifactKind, WireSchemaVersion, WIRE_SCHEMA_VERSION,
};
pub use derived::{
    decode_derived_artifact_envelope_json, DerivedArtifactEnvelope, DerivedArtifactFields,
    DerivedArtifactKind,
};
pub use envelopes::{
    decode_context_pack_envelope_json, decode_evidence_pack_envelope_json,
    decode_query_plan_envelope_json, decode_workflow_envelope_json, ContextPackEnvelope,
    ContextPackFields, EvidencePackEnvelope, EvidencePackFields, QueryPlanEnvelope,
    QueryPlanFields, WorkflowEnvelope, WorkflowEnvelopeFields, WorkflowPhase,
};
pub use identity::{
    CanonicalIdentity, CanonicalIdentityFields, ContentHash, WireEnvelopeHeader,
    WireEnvelopeHeaderFields,
};
pub use ser::{
    decode_wire_document, encode_wire_document, encode_wire_document_pretty, verify_content_hash,
    wire_content_hash,
};

#[cfg(test)]
#[path = "../wire_schemas_tests.rs"]
mod tests;
