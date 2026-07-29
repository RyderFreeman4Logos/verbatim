//! Integration tests for the Tantivy lexical engine contract (Refs #380).
//!
//! These tests exercise cross-module invariants that unit tests in each
//! submodule do not cover: serialization round-trips, redaction guarantees
//! across the full contract surface, and end-to-end generation construction.

use serde_json;

use crate::lexical_engine::conformance::{
    ConformanceMetric, ConformanceObservations, ConformanceSuiteId, ConformanceThreshold,
    LexicalConformanceGate,
};
use crate::lexical_engine::generation::{
    CorpusStatsHash, CorpusStatsSnapshot, LexicalGeneration, LexicalGenerationId,
};
use crate::lexical_engine::migration::{
    reject_mixed_generation_read, AnalyzerChangeDisclosure, AnalyzerChangeReason, BackendClass,
    NonCanonicalMigrationContract, SemanticDifferenceDisclosure,
};
use crate::lexical_engine::retrievers::{CompletenessClaim, LexicalRetrieverType};
use crate::lexical_engine::schema::{
    AnalyzerFamily, AnalyzerIdentity, AnalyzerVariant, FieldIndexingFlags, LexicalFieldSpec,
    LexicalFieldType, LexicalSchema,
};
use crate::lexical_engine::scoring::{
    Bm25ScoringConfig, FieldCombinationStrategy, IdfScope, LengthNormalizationStrategy,
};
use crate::lexical_engine::{
    LexicalEngineDiagnosticCode, LexicalEngineError, LEXICAL_ENGINE_CONTRACT_SCHEMA_VERSION,
};

fn valid_hash() -> CorpusStatsHash {
    CorpusStatsHash::new(format!("sha256:{}", "a".repeat(64))).unwrap()
}

fn english_analyzer() -> AnalyzerIdentity {
    AnalyzerIdentity::new(AnalyzerFamily::English, AnalyzerVariant::Standard, 1).unwrap()
}

fn enterprise_schema() -> LexicalSchema {
    let fields = vec![
        LexicalFieldSpec::text("content", None).unwrap(),
        LexicalFieldSpec::text("title", Some(2.0)).unwrap(),
        LexicalFieldSpec::cjk_text("content_cjk", Some(1.5)).unwrap(),
        LexicalFieldSpec::keyword("doc_id").unwrap(),
        LexicalFieldSpec::identifier("checksum").unwrap(),
        LexicalFieldSpec::fast_field("tenant_id", LexicalFieldType::U64).unwrap(),
        LexicalFieldSpec::fast_field("lifecycle", LexicalFieldType::I64).unwrap(),
        LexicalFieldSpec::fast_field("is_active", LexicalFieldType::Bool).unwrap(),
        LexicalFieldSpec::fast_field("ingested_at", LexicalFieldType::Date).unwrap(),
    ];
    LexicalSchema::new(fields, english_analyzer(), 1).unwrap()
}

fn tenant_strict_scoring(schema: &LexicalSchema) -> Bm25ScoringConfig {
    Bm25ScoringConfig::new(
        1.2,
        0.75,
        FieldCombinationStrategy::Bm25F,
        LengthNormalizationStrategy::Uniform,
        IdfScope::PerTenant,
    )
    .unwrap()
    .with_field_boost("title", 3.0, schema)
    .unwrap()
    .with_field_boost("content", 1.0, schema)
    .unwrap()
}

fn enterprise_generation() -> LexicalGeneration {
    let schema = enterprise_schema();
    let scoring = tenant_strict_scoring(&schema);
    let stats = CorpusStatsSnapshot::new(50_000, valid_hash(), IdfScope::PerTenant).unwrap();
    LexicalGeneration::new(LexicalGenerationId::new(1).unwrap(), schema, scoring, stats).unwrap()
}

#[test]
fn enterprise_generation_constructs_successfully() {
    let gen = enterprise_generation();
    assert_eq!(gen.id().value(), 1);
    assert_eq!(gen.schema().version(), 1);
    assert_eq!(gen.scoring().idf_scope(), IdfScope::PerTenant);
    assert_eq!(gen.corpus_stats().document_count(), 50_000);
    assert_eq!(
        gen.contract_version(),
        LEXICAL_ENGINE_CONTRACT_SCHEMA_VERSION
    );
}

#[test]
fn enterprise_schema_has_expected_field_types() {
    let schema = enterprise_schema();
    assert_eq!(schema.fields().len(), 9);
    assert_eq!(schema.tokenized_text_field_count(), 3);
}

#[test]
fn bm25f_combination_is_transparent_not_rrf() {
    let schema = enterprise_schema();
    let scoring = tenant_strict_scoring(&schema);
    assert!(scoring.combination().is_bm25f());
    assert!(!scoring.combination().is_post_scoring_fusion());
}

#[test]
fn weighted_rrf_is_explicitly_not_bm25f() {
    let schema = enterprise_schema();
    let scoring = Bm25ScoringConfig::new(
        1.2,
        0.75,
        FieldCombinationStrategy::WeightedRrf,
        LengthNormalizationStrategy::Uniform,
        IdfScope::PerTenant,
    )
    .unwrap()
    .with_field_boost("title", 3.0, &schema)
    .unwrap();
    assert!(!scoring.combination().is_bm25f());
    assert!(scoring.combination().is_post_scoring_fusion());
}

#[test]
fn error_debug_emits_only_closed_code() {
    let err = LexicalEngineError::contract(LexicalEngineDiagnosticCode::InvalidFieldName);
    let debug = format!("{:?}", err);
    let display = format!("{}", err);
    assert!(debug.contains("invalid_field_name"));
    assert!(display.contains("lexical-engine.invalid_field_name"));
    // No caller-controlled data can appear because no variant carries any.
    assert!(!debug.contains("secret"));
}

#[test]
fn full_generation_debug_redacts_all_sensitive_data() {
    let gen = enterprise_generation();
    let debug = format!("{:?}", gen);
    // Field names must never appear.
    assert!(!debug.contains("content"));
    assert!(!debug.contains("title"));
    assert!(!debug.contains("tenant_id"));
    assert!(!debug.contains("checksum"));
    // The hash value must never appear.
    assert!(!debug.contains("aaaa"));
}

#[test]
fn schema_serialization_roundtrip() {
    let schema = enterprise_schema();
    let json = serde_json::to_string(&schema).unwrap();
    let deserialized: LexicalSchema = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, schema);
}

#[test]
fn schema_serialization_roundtrip_preserves_boosts() {
    let schema = enterprise_schema();
    let json = serde_json::to_string(&schema).unwrap();
    let deserialized: LexicalSchema = serde_json::from_str(&json).unwrap();
    let title = deserialized
        .fields()
        .iter()
        .find(|f| f.name() == "title")
        .unwrap();
    assert!((title.boost() - 2.0).abs() < f64::EPSILON);
}

#[test]
fn scoring_config_serialization_roundtrip() {
    let schema = enterprise_schema();
    let scoring = tenant_strict_scoring(&schema);
    let json = serde_json::to_string(&scoring).unwrap();
    let deserialized: Bm25ScoringConfig = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, scoring);
}

#[test]
fn generation_serialization_roundtrip() {
    let gen = enterprise_generation();
    let json = serde_json::to_string(&gen).unwrap();
    let deserialized: LexicalGeneration = serde_json::from_str(&json).unwrap();
    assert_eq!(deserialized, gen);
}

#[test]
fn invalid_field_name_rejected_on_deserialize() {
    let bad_json = r#"{
        "name": "bad name with spaces",
        "field_type": "text",
        "indexed": true,
        "stored": true,
        "fast": false,
        "boost": null
    }"#;
    let result: Result<LexicalFieldSpec, _> = serde_json::from_str(bad_json);
    assert!(result.is_err());
}

#[test]
fn conformance_gate_evaluates_candidate() {
    let suite = ConformanceSuiteId::new("lexical-v1", 1).unwrap();
    let thresholds = vec![
        ConformanceThreshold::new(ConformanceMetric::NdcgAtK, 0.6).unwrap(),
        ConformanceThreshold::new(ConformanceMetric::RecallAtK, 0.7).unwrap(),
    ];
    let gate = LexicalConformanceGate::new(suite, thresholds, 500).unwrap();

    // Candidate passes.
    let passing = ConformanceObservations::new()
        .record(ConformanceMetric::NdcgAtK, 0.65)
        .unwrap()
        .record(ConformanceMetric::RecallAtK, 0.75)
        .unwrap();
    assert!(gate.evaluate(&passing).is_ok());

    // Candidate fails.
    let failing = ConformanceObservations::new()
        .record(ConformanceMetric::NdcgAtK, 0.55)
        .unwrap()
        .record(ConformanceMetric::RecallAtK, 0.75)
        .unwrap();
    assert!(gate.evaluate(&failing).is_err());
}

#[test]
fn non_canonical_migration_requires_disclosure_and_gate() {
    let suite = ConformanceSuiteId::new("lexical-v1", 1).unwrap();
    let thresholds = vec![ConformanceThreshold::new(ConformanceMetric::NdcgAtK, 0.6).unwrap()];
    let gate = LexicalConformanceGate::new(suite, thresholds, 500).unwrap();
    let disclosure =
        SemanticDifferenceDisclosure::new(true, true, true, false, true, true).unwrap();
    let contract =
        NonCanonicalMigrationContract::new(BackendClass::QdrantSparse, disclosure, gate).unwrap();
    assert_eq!(contract.candidate(), BackendClass::QdrantSparse);
    assert!(contract.not_transparent());
    assert!(contract.disclosure().tokenizer_differs());
}

#[test]
fn bm25_top_k_cannot_justify_completeness() {
    let retriever = LexicalRetrieverType::Bm25TopK;
    for claim in [
        CompletenessClaim::All,
        CompletenessClaim::Only,
        CompletenessClaim::None,
        CompletenessClaim::Every,
    ] {
        assert!(
            retriever.validate_completeness_claim(claim).is_err(),
            "Bm25TopK must not justify {:?}",
            claim
        );
    }
}

#[test]
fn exhaustive_enumeration_justifies_completeness() {
    let retriever = LexicalRetrieverType::ExhaustiveEnumeration;
    for claim in [
        CompletenessClaim::All,
        CompletenessClaim::Only,
        CompletenessClaim::None,
        CompletenessClaim::Every,
    ] {
        assert!(
            retriever.validate_completeness_claim(claim).is_ok(),
            "ExhaustiveEnumeration should justify {:?}",
            claim
        );
    }
}

#[test]
fn reject_mixed_generation_read_blocks_cross_generation_query() {
    assert!(reject_mixed_generation_read(1, 1).is_ok());
    let err = reject_mixed_generation_read(2, 1).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        LexicalEngineDiagnosticCode::MixedGenerationRead
    );
}

#[test]
fn tenant_strict_ranking_requires_isolated_idf_scope() {
    let schema = enterprise_schema();
    let global_scoring = Bm25ScoringConfig::new(
        1.2,
        0.75,
        FieldCombinationStrategy::Bm25F,
        LengthNormalizationStrategy::Uniform,
        IdfScope::Global,
    )
    .unwrap();
    let err = global_scoring.validate_tenant_strictness(true).unwrap_err();
    assert_eq!(
        err.diagnostic_code(),
        LexicalEngineDiagnosticCode::IncompatibleIdfScope
    );

    let tenant_scoring = tenant_strict_scoring(&schema);
    assert!(tenant_scoring.validate_tenant_strictness(true).is_ok());
}

#[test]
fn analyzer_change_requires_reindex_disclosure() {
    let disclosure = AnalyzerChangeDisclosure::new(AnalyzerChangeReason::TokenizerChanged, true);
    assert!(disclosure.requires_reindex());
    assert_eq!(disclosure.reason(), AnalyzerChangeReason::TokenizerChanged);
}

#[test]
fn field_flags_classify_correctly() {
    assert!(FieldIndexingFlags::indexed().is_indexed());
    assert!(!FieldIndexingFlags::indexed().is_stored());
    assert!(FieldIndexingFlags::stored().is_stored());
    assert!(!FieldIndexingFlags::stored().is_indexed());
    assert!(FieldIndexingFlags::indexed_stored().is_indexed());
    assert!(FieldIndexingFlags::indexed_stored().is_stored());
    assert!(FieldIndexingFlags::fast().is_fast());
    assert!(FieldIndexingFlags::fast().is_indexed());
}
