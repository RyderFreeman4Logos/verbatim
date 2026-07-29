//! Non-canonical backend migration contract for the lexical engine (Refs #380).
//!
//! Qdrant sparse search and LanceDB FTS are useful comparison or co-located
//! deployment options, but they are **not canonical** and migration is not
//! transparent. This module defines the disclosure contract that must accompany
//! any non-canonical backend evaluation or migration: tokenizer, stemming,
//! stop-word, position, field-scoring, and IDF-scope differences must be
//! explicitly disclosed, and the candidate must pass the conformance/qrel suite.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::conformance::LexicalConformanceGate;
use super::error::{LexicalEngineDiagnosticCode, LexicalEngineError, LexicalEngineResult};

/// The canonical (authoritative) lexical backend: Tantivy.
pub const CANONICAL_BACKEND: BackendClass = BackendClass::Tantivy;

/// Closed set of lexical backend classes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendClass {
    /// Tantivy — the canonical enterprise lexical engine.
    Tantivy,
    /// Qdrant sparse-vector BM25 (non-canonical).
    QdrantSparse,
    /// LanceDB FTS (non-canonical).
    LanceDbFts,
    /// SQLite FTS5 (non-canonical).
    SqliteFts5,
}

impl BackendClass {
    /// Returns `true` if this backend is the canonical Tantivy engine.
    pub const fn is_canonical(self) -> bool {
        matches!(self, Self::Tantivy)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Tantivy => "tantivy",
            Self::QdrantSparse => "qdrant_sparse",
            Self::LanceDbFts => "lancedb_fts",
            Self::SqliteFts5 => "sqlite_fts5",
        }
    }
}

/// Disclosed semantic differences between the canonical and a candidate
/// non-canonical backend.
///
/// Migration is not transparent because any of these can differ. Each flag is
/// explicitly declared so the difference is auditable rather than silent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SemanticDifferenceDisclosure {
    tokenizer_differs: bool,
    stemming_differs: bool,
    stop_words_differ: bool,
    positions_differs: bool,
    field_scoring_differs: bool,
    idf_scope_differs: bool,
}

impl SemanticDifferenceDisclosure {
    /// Constructs a disclosure. At least one difference must be declared for a
    /// non-canonical backend (migration is never transparent).
    pub fn new(
        tokenizer_differs: bool,
        stemming_differs: bool,
        stop_words_differ: bool,
        positions_differs: bool,
        field_scoring_differs: bool,
        idf_scope_differs: bool,
    ) -> LexicalEngineResult<Self> {
        let any = tokenizer_differs
            || stemming_differs
            || stop_words_differ
            || positions_differs
            || field_scoring_differs
            || idf_scope_differs;
        if !any {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::NonCanonicalMigrationUndisclosed,
            ));
        }
        Ok(Self {
            tokenizer_differs,
            stemming_differs,
            stop_words_differ,
            positions_differs,
            field_scoring_differs,
            idf_scope_differs,
        })
    }

    pub const fn tokenizer_differs(self) -> bool {
        self.tokenizer_differs
    }

    pub const fn stemming_differs(self) -> bool {
        self.stemming_differs
    }

    pub const fn stop_words_differ(self) -> bool {
        self.stop_words_differ
    }

    pub const fn positions_differs(self) -> bool {
        self.positions_differs
    }

    pub const fn field_scoring_differs(self) -> bool {
        self.field_scoring_differs
    }

    pub const fn idf_scope_differs(self) -> bool {
        self.idf_scope_differs
    }
}

/// Disclosure that an analyzer change within Tantivy requires a staged rebuild.
///
/// This is the intra-canonical analogue of [`SemanticDifferenceDisclosure`]:
/// changing the analyzer pipeline (tokenization, stemming, stop words,
/// positions, normalization) within Tantivy still requires a generation bump
/// and atomic cutover.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AnalyzerChangeDisclosure {
    /// Human-readable closed reason code (validated, not caller-free-text).
    reason: AnalyzerChangeReason,
    /// Whether a full re-index is required.
    requires_reindex: bool,
}

impl AnalyzerChangeDisclosure {
    /// Constructs an analyzer-change disclosure.
    pub fn new(reason: AnalyzerChangeReason, requires_reindex: bool) -> Self {
        Self {
            reason,
            requires_reindex,
        }
    }

    pub const fn reason(&self) -> AnalyzerChangeReason {
        self.reason
    }

    pub const fn requires_reindex(&self) -> bool {
        self.requires_reindex
    }
}

/// Closed set of analyzer-change reason codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzerChangeReason {
    /// Tokenizer pipeline changed.
    TokenizerChanged,
    /// Stemming algorithm or dictionary changed.
    StemmingChanged,
    /// Stop-word list changed.
    StopWordsChanged,
    /// Position recording changed (affects phrase queries).
    PositionsChanged,
    /// Normalization (lowercasing, accent folding) changed.
    NormalizationChanged,
}

/// A non-canonical backend migration evaluation contract.
///
/// This binds a candidate backend, its disclosed semantic differences, and the
/// conformance gate it must pass before cutover. It explicitly records that the
/// migration is **not transparent**.
#[derive(Clone, Serialize, Deserialize)]
pub struct NonCanonicalMigrationContract {
    candidate: BackendClass,
    disclosure: SemanticDifferenceDisclosure,
    conformance_gate: LexicalConformanceGate,
    /// Explicit acknowledgment that migration is not transparent.
    not_transparent: bool,
}

impl NonCanonicalMigrationContract {
    /// Constructs a non-canonical migration contract.
    pub fn new(
        candidate: BackendClass,
        disclosure: SemanticDifferenceDisclosure,
        conformance_gate: LexicalConformanceGate,
    ) -> LexicalEngineResult<Self> {
        if candidate.is_canonical() {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidContract,
            ));
        }
        Ok(Self {
            candidate,
            disclosure,
            conformance_gate,
            not_transparent: true,
        })
    }

    pub const fn candidate(&self) -> BackendClass {
        self.candidate
    }

    pub fn disclosure(&self) -> &SemanticDifferenceDisclosure {
        &self.disclosure
    }

    pub fn conformance_gate(&self) -> &LexicalConformanceGate {
        &self.conformance_gate
    }

    pub const fn not_transparent(&self) -> bool {
        self.not_transparent
    }
}

impl fmt::Debug for NonCanonicalMigrationContract {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("NonCanonicalMigrationContract")
            .field("candidate", &self.candidate)
            .field("disclosure", &self.disclosure)
            .field("not_transparent", &self.not_transparent)
            .finish()
    }
}

/// Rejects a mixed-generation read: a single query path must not read from two
/// lexical generations simultaneously.
///
/// This mirrors the generation-publication contract's
/// `reject_mixed_generation_read` for the lexical engine specifically.
pub fn reject_mixed_generation_read(active: u64, requested: u64) -> LexicalEngineResult<()> {
    if active != requested {
        return Err(LexicalEngineError::contract(
            LexicalEngineDiagnosticCode::MixedGenerationRead,
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexical_engine::conformance::{
        ConformanceMetric, ConformanceObservations, ConformanceSuiteId, ConformanceThreshold,
    };

    fn gate() -> LexicalConformanceGate {
        let suite = ConformanceSuiteId::new("lexical-v1", 1).unwrap();
        let thresholds = vec![ConformanceThreshold::new(ConformanceMetric::NdcgAtK, 0.5).unwrap()];
        LexicalConformanceGate::new(suite, thresholds, 100).unwrap()
    }

    #[test]
    fn tantivy_is_canonical() {
        assert!(BackendClass::Tantivy.is_canonical());
        assert!(!BackendClass::QdrantSparse.is_canonical());
        assert!(!BackendClass::LanceDbFts.is_canonical());
    }

    #[test]
    fn disclosure_requires_at_least_one_difference() {
        let err = SemanticDifferenceDisclosure::new(false, false, false, false, false, false)
            .unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::NonCanonicalMigrationUndisclosed
        );
    }

    #[test]
    fn disclosure_accepts_declared_differences() {
        let d = SemanticDifferenceDisclosure::new(true, false, false, false, false, false).unwrap();
        assert!(d.tokenizer_differs());
    }

    #[test]
    fn migration_contract_rejects_canonical_candidate() {
        let disclosure =
            SemanticDifferenceDisclosure::new(true, false, false, false, false, false).unwrap();
        let err = NonCanonicalMigrationContract::new(BackendClass::Tantivy, disclosure, gate())
            .unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::InvalidContract
        );
    }

    #[test]
    fn migration_contract_accepts_non_canonical() {
        let disclosure =
            SemanticDifferenceDisclosure::new(true, true, false, true, true, false).unwrap();
        let contract =
            NonCanonicalMigrationContract::new(BackendClass::QdrantSparse, disclosure, gate())
                .unwrap();
        assert_eq!(contract.candidate(), BackendClass::QdrantSparse);
        assert!(contract.not_transparent());
    }

    #[test]
    fn reject_mixed_generation_read_accepts_same() {
        assert!(reject_mixed_generation_read(1, 1).is_ok());
    }

    #[test]
    fn reject_mixed_generation_read_rejects_different() {
        let err = reject_mixed_generation_read(1, 2).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::MixedGenerationRead
        );
    }

    #[test]
    fn analyzer_change_disclosure() {
        let d = AnalyzerChangeDisclosure::new(AnalyzerChangeReason::TokenizerChanged, true);
        assert_eq!(d.reason(), AnalyzerChangeReason::TokenizerChanged);
        assert!(d.requires_reindex());
    }

    #[test]
    fn conformance_observations_record() {
        let obs = ConformanceObservations::new()
            .record(ConformanceMetric::NdcgAtK, 0.8)
            .unwrap();
        assert_eq!(obs.len(), 1);
        assert!(obs.get(ConformanceMetric::NdcgAtK).is_some());
    }
}
