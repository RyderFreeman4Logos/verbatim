//! Lexical generation: schema + analyzer identity + corpus statistics
//! snapshot, versioned and bound to a `RetrievalProfile` (Refs #380).
//!
//! A [`LexicalGeneration`] is the immutable identity of a built lexical index.
//! It ties together the schema, analyzer identity, BM25 scoring config, IDF
//! scope, and a corpus-statistics snapshot. Changes to any component require a
//! new generation with staged rebuild and atomic cutover.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::error::{LexicalEngineDiagnosticCode, LexicalEngineError, LexicalEngineResult};
use super::schema::{AnalyzerIdentity, LexicalSchema};
use super::scoring::{Bm25ScoringConfig, IdfScope};

/// Schema version for lexical-generation contract documents.
pub const LEXICAL_GENERATION_SCHEMA_VERSION: u32 = 1;

/// Validated `sha256:` content hash for a corpus-statistics snapshot or lexical
/// artifact. The value is retained for integrity comparison but never rendered
/// in diagnostics.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct CorpusStatsHash(String);

impl CorpusStatsHash {
    /// Constructs a validated `sha256:` hex digest.
    pub fn new(value: impl Into<String>) -> LexicalEngineResult<Self> {
        let hash = Self(value.into());
        hash.validate()?;
        Ok(hash)
    }

    /// Returns the serialized hash.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Revalidates the `sha256:` prefix and 64 lowercase-hex digits.
    pub fn validate(&self) -> LexicalEngineResult<()> {
        let valid = self
            .0
            .strip_prefix("sha256:")
            .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
        if !valid {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidHash,
            ));
        }
        Ok(())
    }
}

impl TryFrom<String> for CorpusStatsHash {
    type Error = LexicalEngineError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl fmt::Debug for CorpusStatsHash {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("CorpusStatsHash(REDACTED)")
    }
}

/// Snapshot of corpus statistics at generation-build time.
///
/// This records the document count, total token count, and average field
/// lengths used for BM25 length normalization. The IDF scope determines
/// whether these statistics are global, per-tenant, per-collection, or
/// segmented.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusStatsSnapshot {
    /// Number of documents in the generation's corpus.
    document_count: u64,
    /// Content hash of the full statistics blob (term frequencies, field
    /// lengths). Redacted in Debug via [`CorpusStatsHash`].
    stats_hash: CorpusStatsHash,
    /// IDF scope under which these statistics were computed.
    idf_scope: IdfScope,
}

impl CorpusStatsSnapshot {
    /// Constructs a corpus statistics snapshot.
    pub fn new(
        document_count: u64,
        stats_hash: CorpusStatsHash,
        idf_scope: IdfScope,
    ) -> LexicalEngineResult<Self> {
        if document_count == 0 {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidBounds,
            ));
        }
        Ok(Self {
            document_count,
            stats_hash,
            idf_scope,
        })
    }

    pub const fn document_count(&self) -> u64 {
        self.document_count
    }

    pub fn stats_hash(&self) -> &CorpusStatsHash {
        &self.stats_hash
    }

    pub const fn idf_scope(&self) -> IdfScope {
        self.idf_scope
    }
}

/// Monotonic, nonzero lexical generation identifier.
///
/// Only one lexical generation is active for query serving at a time; older
/// generations remain readable during migration shadow evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct LexicalGenerationId(u64);

impl<'de> Deserialize<'de> for LexicalGenerationId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl LexicalGenerationId {
    /// Constructs a nonzero lexical generation id.
    pub fn new(value: u64) -> LexicalEngineResult<Self> {
        if value == 0 {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the serialized generation value.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// The immutable identity of a built lexical index generation.
///
/// This ties together the schema, analyzer identity, BM25 scoring config, and
/// corpus statistics snapshot. It is part of the `RetrievalProfile`. Any change
/// to schema, analyzer, scoring, or corpus requires a new generation.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct LexicalGeneration {
    id: LexicalGenerationId,
    schema: LexicalSchema,
    analyzer: AnalyzerIdentity,
    scoring: Bm25ScoringConfig,
    corpus_stats: CorpusStatsSnapshot,
    /// Schema version of this contract document.
    contract_version: u32,
}

impl LexicalGeneration {
    /// Constructs a new lexical generation, validating internal consistency.
    pub fn new(
        id: LexicalGenerationId,
        schema: LexicalSchema,
        scoring: Bm25ScoringConfig,
        corpus_stats: CorpusStatsSnapshot,
    ) -> LexicalEngineResult<Self> {
        // The analyzer identity from the schema is authoritative.
        let analyzer = schema.analyzer().clone();
        // The IDF scope in scoring must match the corpus stats scope.
        if scoring.idf_scope() != corpus_stats.idf_scope() {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::IncompatibleIdfScope,
            ));
        }
        Ok(Self {
            id,
            schema,
            analyzer,
            scoring,
            corpus_stats,
            contract_version: LEXICAL_GENERATION_SCHEMA_VERSION,
        })
    }

    pub const fn id(&self) -> LexicalGenerationId {
        self.id
    }

    pub fn schema(&self) -> &LexicalSchema {
        &self.schema
    }

    pub const fn analyzer(&self) -> &AnalyzerIdentity {
        &self.analyzer
    }

    pub fn scoring(&self) -> &Bm25ScoringConfig {
        &self.scoring
    }

    pub fn corpus_stats(&self) -> &CorpusStatsSnapshot {
        &self.corpus_stats
    }

    pub const fn contract_version(&self) -> u32 {
        self.contract_version
    }

    /// Validates that this generation's analyzer identity matches an expected
    /// identity (e.g. when binding a query to a generation).
    pub fn validate_analyzer_match(&self, expected: &AnalyzerIdentity) -> LexicalEngineResult<()> {
        if self.analyzer != *expected {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::AnalyzerMismatch,
            ));
        }
        Ok(())
    }

    /// Validates that this generation's schema version matches an expected
    /// version.
    pub fn validate_schema_version(&self, expected: u32) -> LexicalEngineResult<()> {
        if self.schema.version() != expected {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::SchemaVersionMismatch,
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for LexicalGeneration {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redacted: emit only structural metadata, never field names or hashes.
        formatter
            .debug_struct("LexicalGeneration")
            .field("id", &self.id)
            .field("schema", &self.schema)
            .field("analyzer", &self.analyzer)
            .field("scoring", &self.scoring)
            .field("corpus_stats", &self.corpus_stats)
            .field("contract_version", &self.contract_version)
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexical_engine::schema::{
        AnalyzerFamily, AnalyzerIdentity, AnalyzerVariant, LexicalFieldSpec,
    };
    use crate::lexical_engine::scoring::{FieldCombinationStrategy, LengthNormalizationStrategy};

    fn valid_hash() -> CorpusStatsHash {
        CorpusStatsHash::new("sha256:".to_string() + &"a".repeat(64)).unwrap()
    }

    fn base_generation() -> LexicalGeneration {
        let analyzer =
            AnalyzerIdentity::new(AnalyzerFamily::English, AnalyzerVariant::Standard, 1).unwrap();
        let fields = vec![LexicalFieldSpec::text("content", None).unwrap()];
        let schema = LexicalSchema::new(fields, analyzer, 1).unwrap();
        let scoring = Bm25ScoringConfig::new(
            1.2,
            0.75,
            FieldCombinationStrategy::Bm25F,
            LengthNormalizationStrategy::Uniform,
            IdfScope::PerTenant,
        )
        .unwrap();
        let stats = CorpusStatsSnapshot::new(1000, valid_hash(), IdfScope::PerTenant).unwrap();
        LexicalGeneration::new(LexicalGenerationId::new(1).unwrap(), schema, scoring, stats)
            .unwrap()
    }

    #[test]
    fn generation_id_rejects_zero() {
        assert!(LexicalGenerationId::new(0).is_err());
        assert!(LexicalGenerationId::new(1).is_ok());
    }

    #[test]
    fn corpus_stats_rejects_zero_docs() {
        let err = CorpusStatsSnapshot::new(0, valid_hash(), IdfScope::Global).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::InvalidBounds
        );
    }

    #[test]
    fn corpus_stats_hash_rejects_invalid() {
        assert!(CorpusStatsHash::new("not-a-hash").is_err());
        assert!(CorpusStatsHash::new("sha256:short").is_err());
        assert!(valid_hash().validate().is_ok());
    }

    #[test]
    fn corpus_stats_hash_debug_redacted() {
        let hash = valid_hash();
        let debug = format!("{:?}", hash);
        assert!(debug.contains("REDACTED"));
        assert!(!debug.contains("aaaa"));
    }

    #[test]
    fn generation_rejects_mismatched_idf_scope() {
        let analyzer =
            AnalyzerIdentity::new(AnalyzerFamily::English, AnalyzerVariant::Standard, 1).unwrap();
        let fields = vec![LexicalFieldSpec::text("content", None).unwrap()];
        let schema = LexicalSchema::new(fields, analyzer, 1).unwrap();
        let scoring = Bm25ScoringConfig::new(
            1.2,
            0.75,
            FieldCombinationStrategy::Bm25F,
            LengthNormalizationStrategy::Uniform,
            IdfScope::Global,
        )
        .unwrap();
        let stats = CorpusStatsSnapshot::new(1000, valid_hash(), IdfScope::PerTenant).unwrap();
        let err =
            LexicalGeneration::new(LexicalGenerationId::new(1).unwrap(), schema, scoring, stats)
                .unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::IncompatibleIdfScope
        );
    }

    #[test]
    fn generation_accepts_consistent_idf_scope() {
        let gen = base_generation();
        assert_eq!(gen.scoring().idf_scope(), IdfScope::PerTenant);
        assert_eq!(gen.corpus_stats().idf_scope(), IdfScope::PerTenant);
    }

    #[test]
    fn generation_validate_analyzer_match() {
        let gen = base_generation();
        let same = gen.analyzer().clone();
        assert!(gen.validate_analyzer_match(&same).is_ok());

        let different =
            AnalyzerIdentity::new(AnalyzerFamily::English, AnalyzerVariant::Standard, 2).unwrap();
        let err = gen.validate_analyzer_match(&different).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::AnalyzerMismatch
        );
    }

    #[test]
    fn generation_validate_schema_version() {
        let gen = base_generation();
        assert!(gen.validate_schema_version(1).is_ok());
        let err = gen.validate_schema_version(2).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::SchemaVersionMismatch
        );
    }
}
