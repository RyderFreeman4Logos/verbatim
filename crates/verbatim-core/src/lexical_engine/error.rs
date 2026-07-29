//! Fail-closed, diagnostic-only failures for the Tantivy lexical engine
//! contract (Refs #380).
//!
//! No variant retains a caller-controlled field name, tenant, ACL principal,
//! source id, collection id, analyzer identity, tokenizer name, qrel label,
//! or content hash. Public `Debug` and `Display` emit only the closed code, so
//! a failure is safe to surface in operational diagnostics.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Result alias for lexical-engine contract operations.
pub type LexicalEngineResult<T> = Result<T, LexicalEngineError>;

/// Closed diagnostic taxonomy. No variant retains caller-controlled input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LexicalEngineDiagnosticCode {
    /// A schema, generation, or contract document was malformed or internally
    /// inconsistent.
    InvalidContract,
    /// A lexical field name, type, or flag combination was invalid.
    InvalidFieldSpec,
    /// A field name was empty, blank, or longer than the bound.
    InvalidFieldName,
    /// A duplicate field name appeared within one schema.
    DuplicateField,
    /// A BM25 parameter (k1, b, boost, per-field boost) was out of range.
    InvalidScoring,
    /// The field-combination strategy was unsupported or ambiguous.
    InvalidFieldCombination,
    /// An IDF scope conflicted with a strict tenant-ranking contract.
    IncompatibleIdfScope,
    /// A generation, version, or analyzer-identity value was zero or invalid.
    InvalidIdentity,
    /// A content hash failed `sha256:` revalidation.
    InvalidHash,
    /// A bounded count, list, or qrel suite was empty or exceeded its bound.
    InvalidBounds,
    /// A required schema, analyzer, or corpus-statistics component was missing.
    MissingComponent,
    /// The schema version did not match the generation's bound version.
    SchemaVersionMismatch,
    /// The analyzer identity did not match the generation's bound identity.
    AnalyzerMismatch,
    /// The requested retriever type cannot justify a completeness claim.
    UnsupportedCompletenessClaim,
    /// A conformance/qrel gate failed before publication or migration.
    ConformanceGateFailed,
    /// A non-canonical backend (Qdrant/LanceDB) migration was attempted
    /// without a disclosed semantic-difference disclosure.
    NonCanonicalMigrationUndisclosed,
    /// Mixed lexical generations were read in a single query path.
    MixedGenerationRead,
    /// JSON encoding or decoding of a contract document failed.
    SerializationFailed,
}

impl LexicalEngineDiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidContract => "invalid_contract",
            Self::InvalidFieldSpec => "invalid_field_spec",
            Self::InvalidFieldName => "invalid_field_name",
            Self::DuplicateField => "duplicate_field",
            Self::InvalidScoring => "invalid_scoring",
            Self::InvalidFieldCombination => "invalid_field_combination",
            Self::IncompatibleIdfScope => "incompatible_idf_scope",
            Self::InvalidIdentity => "invalid_identity",
            Self::InvalidHash => "invalid_hash",
            Self::InvalidBounds => "invalid_bounds",
            Self::MissingComponent => "missing_component",
            Self::SchemaVersionMismatch => "schema_version_mismatch",
            Self::AnalyzerMismatch => "analyzer_mismatch",
            Self::UnsupportedCompletenessClaim => "unsupported_completeness_claim",
            Self::ConformanceGateFailed => "conformance_gate_failed",
            Self::NonCanonicalMigrationUndisclosed => "non_canonical_migration_undisclosed",
            Self::MixedGenerationRead => "mixed_generation_read",
            Self::SerializationFailed => "serialization_failed",
        }
    }
}

/// A lexical-engine contract failure containing only a closed diagnostic code.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum LexicalEngineError {
    Contract { code: LexicalEngineDiagnosticCode },
}

impl LexicalEngineError {
    pub const fn contract(code: LexicalEngineDiagnosticCode) -> Self {
        Self::Contract { code }
    }

    pub const fn diagnostic_code(self) -> LexicalEngineDiagnosticCode {
        match self {
            Self::Contract { code } => code,
        }
    }
}

impl fmt::Debug for LexicalEngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "LexicalEngineError({})",
            self.diagnostic_code().as_str()
        )
    }
}

impl fmt::Display for LexicalEngineError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "lexical-engine.{}",
            self.diagnostic_code().as_str()
        )
    }
}

impl Error for LexicalEngineError {}
