//! Tantivy dedicated enterprise lexical engine: stable BM25 semantics contract
//! (Refs #380).
//!
//! This pure contract module defines the lexical field schema, analyzer
//! identity, BM25 scoring configuration, IDF scope, lexical generation,
//! retriever type classification, conformance/qrel gate, and non-canonical
//! backend migration contract for Tantivy as the dedicated enterprise lexical
//! engine.
//!
//! It deliberately contains **no live Tantivy index, no actual BM25
//! computation, no tokenizer binding, and no query execution**. See
//! `docs/architecture/tantivy-lexical-engine.md`.
//!
//! ## Fail-closed contract surface
//!
//! All validation rejects invalid input. Errors are diagnostic-code-only: no
//! variant retains a caller-controlled field name, tenant, ACL principal,
//! source id, collection id, analyzer identity, tokenizer name, qrel label, or
//! content hash. Public `Debug` and `Display` emit only the closed code.
//!
//! ## Why Tantivy is canonical
//!
//! Tantivy provides a Rust-native inverted index, BM25 scoring, phrases,
//! fields, tokenizers, mmap, incremental indexing, and query primitives
//! suitable for the enterprise lexical role. Qdrant sparse search and LanceDB
//! FTS remain useful comparison or co-located options, but migration is not
//! transparent — see [`migration::SemanticDifferenceDisclosure`].
//!
//! ## Exact and exhaustive paths
//!
//! BM25 Top-K is an approximate relevance ranker and cannot justify `all`,
//! `only`, `none`, or `every` claims. Exact phrase, identifier, reference,
//! metadata, and exhaustive enumeration retrievers are kept separate — see
//! [`retrievers::LexicalRetrieverType`].

mod conformance;
mod error;
mod generation;
mod migration;
mod retrievers;
mod schema;
mod scoring;

pub use conformance::{
    ConformanceMetric, ConformanceObservations, ConformanceSuiteId, ConformanceThreshold,
    LexicalConformanceGate, SuiteName, MAX_QREL_CASES,
};
pub use error::{LexicalEngineDiagnosticCode, LexicalEngineError, LexicalEngineResult};
pub use generation::{
    CorpusStatsHash, CorpusStatsSnapshot, LexicalGeneration, LexicalGenerationId,
    LEXICAL_GENERATION_SCHEMA_VERSION,
};
pub use migration::{
    reject_mixed_generation_read, AnalyzerChangeDisclosure, AnalyzerChangeReason, BackendClass,
    NonCanonicalMigrationContract, SemanticDifferenceDisclosure, CANONICAL_BACKEND,
};
pub use retrievers::{CompletenessClaim, LexicalRetrieverType};
pub use schema::{
    AnalyzerFamily, AnalyzerIdentity, AnalyzerVariant, FieldIndexingFlags, FieldName,
    LexicalFieldSpec, LexicalFieldType, LexicalSchema, FIELD_NAME_MAX_LEN, SCHEMA_MAX_FIELDS,
};
pub use scoring::{
    Bm25ScoringConfig, FieldCombinationStrategy, IdfScope, LengthNormalizationStrategy, BM25_B_MAX,
    BM25_B_MIN, BM25_K1_MAX, BM25_K1_MIN, FIELD_BOOST_MAX, FIELD_BOOST_MIN, MAX_FIELD_BOOSTS,
};

/// Contract schema version for lexical-engine documents.
pub const LEXICAL_ENGINE_CONTRACT_SCHEMA_VERSION: u32 = LEXICAL_GENERATION_SCHEMA_VERSION;

#[cfg(test)]
#[path = "../lexical_engine_tests.rs"]
mod tests;
