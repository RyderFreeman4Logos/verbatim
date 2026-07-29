//! BM25 scoring configuration, field-combination strategy, and IDF scope for
//! the Tantivy lexical engine (Refs #380).
//!
//! These types pin the exact BM25 semantics (k1, b, per-field boosts, length
//! normalization) and the corpus/IDF statistics scope. The key contract is that
//! a field-combination strategy is *explicit and transparent* — weighted RRF
//! over separate fields is **not** assumed to be mathematically identical to
//! BM25F. See `docs/architecture/tantivy-lexical-engine.md`.

use std::collections::HashMap;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::error::{LexicalEngineDiagnosticCode, LexicalEngineError, LexicalEngineResult};
use super::schema::{LexicalSchema, FIELD_NAME_MAX_LEN};

/// Lower bound for the BM25 `k1` term-frequency saturation parameter.
pub const BM25_K1_MIN: f64 = 0.0;
/// Upper bound for the BM25 `k1` parameter.
pub const BM25_K1_MAX: f64 = 10.0;
/// Lower bound for the BM25 `b` length-normalization parameter.
pub const BM25_B_MIN: f64 = 0.0;
/// Upper bound for the BM25 `b` parameter.
pub const BM25_B_MAX: f64 = 1.0;
/// Lower bound for a per-field boost.
pub const FIELD_BOOST_MIN: f64 = 0.0;
/// Upper bound for a per-field boost.
pub const FIELD_BOOST_MAX: f64 = 100.0;
/// Maximum number of per-field boosts recorded.
pub const MAX_FIELD_BOOSTS: usize = 256;

/// Corpus/IDF statistics scope.
///
/// Records whether document-frequency / corpus-length statistics are global,
/// per tenant, per collection, or segmented. In multi-tenant deployments, the
/// scope must be disclosed so that other tenants' statistics do not silently
/// alter a strict tenant-specific ranking contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IdfScope {
    /// Statistics aggregated over the entire corpus (all tenants).
    Global,
    /// Statistics scoped to a single tenant (strict tenant ranking).
    PerTenant,
    /// Statistics scoped to a single collection within a tenant.
    PerCollection,
    /// Statistics segmented (e.g. per shard / per segment); segments must be
    /// merged consistently before ranking.
    Segmented,
}

impl IdfScope {
    /// Returns `true` if this scope isolates one tenant's statistics from
    /// another tenant's documents (i.e. is safe for strict tenant ranking).
    pub const fn is_tenant_isolated(self) -> bool {
        matches!(self, Self::PerTenant | Self::PerCollection)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Global => "global",
            Self::PerTenant => "per_tenant",
            Self::PerCollection => "per_collection",
            Self::Segmented => "segmented",
        }
    }
}

/// How multiple tokenized text fields are combined into a single ranking score.
///
/// Weighted RRF over separate per-field indexes is **not** BM25F. If BM25F-like
/// semantics are desired, the strategy must be declared explicitly so the
/// difference is auditable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FieldCombinationStrategy {
    /// True BM25F: per-field term frequencies and lengths are combined inside
    /// the saturation function before scoring (field-combination *inside* the
    /// BM25 formula).
    Bm25F,
    /// Per-field BM25 scores combined by a weighted sum after scoring.
    WeightedSum,
    /// Per-field BM25 result lists combined by Reciprocal Rank Fusion (RRF).
    /// Explicitly **not** mathematically identical to BM25F.
    WeightedRrf,
}

impl FieldCombinationStrategy {
    /// Returns `true` if this strategy is the transparent BM25F combination.
    pub const fn is_bm25f(self) -> bool {
        matches!(self, Self::Bm25F)
    }

    /// Returns `true` if this strategy is a post-scoring fusion (WeightedSum or
    /// WeightedRrf), which differs from true BM25F.
    pub const fn is_post_scoring_fusion(self) -> bool {
        matches!(self, Self::WeightedSum | Self::WeightedRrf)
    }

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Bm25F => "bm25f",
            Self::WeightedSum => "weighted_sum",
            Self::WeightedRrf => "weighted_rrf",
        }
    }
}

/// Length-normalization behavior for BM25.
///
/// This is bound by the `b` parameter: `b=0` disables length normalization;
/// `b=1` applies full normalization. The strategy records whether per-field
/// length normalization is applied uniformly or per-field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LengthNormalizationStrategy {
    /// A single global `b` parameter applies to all tokenized text fields.
    Uniform,
    /// A per-field `b` parameter applies to each tokenized text field.
    PerField,
}

/// BM25 scoring configuration with explicit field boosts and length
/// normalization.
///
/// This pins the exact ranking semantics for a lexical generation. Changes
/// require a generation bump and re-evaluation against the conformance/qrel
/// suite.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Bm25ScoringConfig {
    k1: f64,
    b: f64,
    combination: FieldCombinationStrategy,
    length_norm: LengthNormalizationStrategy,
    idf_scope: IdfScope,
    /// Per-field boosts keyed by field name (validated, redacted in Debug).
    field_boosts: HashMap<String, f64>,
}

impl Bm25ScoringConfig {
    /// Constructs a new BM25 scoring configuration.
    pub fn new(
        k1: f64,
        b: f64,
        combination: FieldCombinationStrategy,
        length_norm: LengthNormalizationStrategy,
        idf_scope: IdfScope,
    ) -> LexicalEngineResult<Self> {
        Self::validate_k1(k1)?;
        Self::validate_b(b)?;
        Ok(Self {
            k1,
            b,
            combination,
            length_norm,
            idf_scope,
            field_boosts: HashMap::new(),
        })
    }

    fn validate_k1(k1: f64) -> LexicalEngineResult<()> {
        if !k1.is_finite() || !(BM25_K1_MIN..=BM25_K1_MAX).contains(&k1) {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidScoring,
            ));
        }
        Ok(())
    }

    fn validate_b(b: f64) -> LexicalEngineResult<()> {
        if !b.is_finite() || !(BM25_B_MIN..=BM25_B_MAX).contains(&b) {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidScoring,
            ));
        }
        Ok(())
    }

    /// Sets a per-field boost for a named field.
    ///
    /// The field name is validated to the same closed character set as schema
    /// field names. The boost must be a positive finite value within bounds.
    /// Only tokenized text fields (present in the schema) may receive a boost.
    pub fn with_field_boost(
        mut self,
        field: impl Into<String>,
        boost: f64,
        schema: &LexicalSchema,
    ) -> LexicalEngineResult<Self> {
        let name = field.into();
        Self::validate_field_name(&name)?;
        if !boost.is_finite() || !(FIELD_BOOST_MIN..=FIELD_BOOST_MAX).contains(&boost) {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidScoring,
            ));
        }
        if self.field_boosts.len() >= MAX_FIELD_BOOSTS {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidBounds,
            ));
        }
        // The field must exist in the schema and be a tokenized text field.
        let found = schema
            .fields()
            .iter()
            .find(|f| f.name() == name)
            .ok_or_else(|| {
                LexicalEngineError::contract(LexicalEngineDiagnosticCode::InvalidFieldSpec)
            })?;
        if !found.field_type().is_tokenized_text() {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidFieldSpec,
            ));
        }
        self.field_boosts.insert(name, boost);
        Ok(self)
    }

    fn validate_field_name(name: &str) -> LexicalEngineResult<()> {
        if name.is_empty() || name.len() > FIELD_NAME_MAX_LEN {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidFieldName,
            ));
        }
        let valid = name
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            && name
                .bytes()
                .skip(1)
                .all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if !valid {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidFieldName,
            ));
        }
        Ok(())
    }

    pub const fn k1(&self) -> f64 {
        self.k1
    }

    pub const fn b(&self) -> f64 {
        self.b
    }

    pub const fn combination(&self) -> FieldCombinationStrategy {
        self.combination
    }

    pub const fn length_normalization(&self) -> LengthNormalizationStrategy {
        self.length_norm
    }

    pub const fn idf_scope(&self) -> IdfScope {
        self.idf_scope
    }

    /// Returns the boost for a field, defaulting to 1.0.
    pub fn field_boost(&self, field: &str) -> f64 {
        self.field_boosts.get(field).copied().unwrap_or(1.0)
    }

    /// Returns the number of explicitly-set per-field boosts.
    pub fn field_boost_count(&self) -> usize {
        self.field_boosts.len()
    }

    /// Returns `true` if per-field boosts differ from the default (1.0).
    pub fn has_custom_boosts(&self) -> bool {
        self.field_boosts
            .values()
            .any(|&v| (v - 1.0).abs() > f64::EPSILON)
    }

    /// Validates that this scoring config is compatible with a strict
    /// tenant-ranking contract: a `Global` IDF scope is incompatible with
    /// strict tenant ranking unless explicitly disclosed.
    pub fn validate_tenant_strictness(
        &self,
        require_tenant_isolation: bool,
    ) -> LexicalEngineResult<()> {
        if require_tenant_isolation && !self.idf_scope.is_tenant_isolated() {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::IncompatibleIdfScope,
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for Bm25ScoringConfig {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redacted: never render field names; emit only the count and params.
        formatter
            .debug_struct("Bm25ScoringConfig")
            .field("k1", &self.k1)
            .field("b", &self.b)
            .field("combination", &self.combination)
            .field("length_norm", &self.length_norm)
            .field("idf_scope", &self.idf_scope)
            .field("field_boost_count", &self.field_boosts.len())
            .finish()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::lexical_engine::schema::{
        AnalyzerFamily, AnalyzerIdentity, AnalyzerVariant, LexicalFieldSpec,
    };

    fn eng_schema() -> LexicalSchema {
        let analyzer =
            AnalyzerIdentity::new(AnalyzerFamily::English, AnalyzerVariant::Standard, 1).unwrap();
        let fields = vec![
            LexicalFieldSpec::text("content", None).unwrap(),
            LexicalFieldSpec::text("title", None).unwrap(),
        ];
        LexicalSchema::new(fields, analyzer, 1).unwrap()
    }

    #[test]
    fn rejects_k1_out_of_range() {
        assert!(Bm25ScoringConfig::new(
            -0.1,
            0.5,
            FieldCombinationStrategy::Bm25F,
            LengthNormalizationStrategy::Uniform,
            IdfScope::Global
        )
        .is_err());
        assert!(Bm25ScoringConfig::new(
            10.1,
            0.5,
            FieldCombinationStrategy::Bm25F,
            LengthNormalizationStrategy::Uniform,
            IdfScope::Global
        )
        .is_err());
    }

    #[test]
    fn rejects_k1_nan() {
        assert!(Bm25ScoringConfig::new(
            f64::NAN,
            0.5,
            FieldCombinationStrategy::Bm25F,
            LengthNormalizationStrategy::Uniform,
            IdfScope::Global
        )
        .is_err());
    }

    #[test]
    fn rejects_b_out_of_range() {
        assert!(Bm25ScoringConfig::new(
            1.2,
            -0.1,
            FieldCombinationStrategy::Bm25F,
            LengthNormalizationStrategy::Uniform,
            IdfScope::Global
        )
        .is_err());
        assert!(Bm25ScoringConfig::new(
            1.2,
            1.1,
            FieldCombinationStrategy::Bm25F,
            LengthNormalizationStrategy::Uniform,
            IdfScope::Global
        )
        .is_err());
    }

    #[test]
    fn accepts_boundary_values() {
        assert!(Bm25ScoringConfig::new(
            0.0,
            0.0,
            FieldCombinationStrategy::Bm25F,
            LengthNormalizationStrategy::Uniform,
            IdfScope::Global
        )
        .is_ok());
        assert!(Bm25ScoringConfig::new(
            10.0,
            1.0,
            FieldCombinationStrategy::Bm25F,
            LengthNormalizationStrategy::Uniform,
            IdfScope::Global
        )
        .is_ok());
    }

    #[test]
    fn field_boost_requires_schema_field() {
        let schema = eng_schema();
        let cfg = Bm25ScoringConfig::new(
            1.2,
            0.75,
            FieldCombinationStrategy::Bm25F,
            LengthNormalizationStrategy::Uniform,
            IdfScope::Global,
        )
        .unwrap();
        let err = cfg
            .with_field_boost("nonexistent", 2.0, &schema)
            .unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::InvalidFieldSpec
        );
    }

    #[test]
    fn field_boost_rejects_non_text_field() {
        let analyzer =
            AnalyzerIdentity::new(AnalyzerFamily::English, AnalyzerVariant::Standard, 1).unwrap();
        let fields = vec![
            LexicalFieldSpec::text("content", None).unwrap(),
            LexicalFieldSpec::fast_field(
                "tenant_id",
                crate::lexical_engine::schema::LexicalFieldType::U64,
            )
            .unwrap(),
        ];
        let schema = LexicalSchema::new(fields, analyzer, 1).unwrap();
        let cfg = Bm25ScoringConfig::new(
            1.2,
            0.75,
            FieldCombinationStrategy::Bm25F,
            LengthNormalizationStrategy::Uniform,
            IdfScope::Global,
        )
        .unwrap();
        let err = cfg.with_field_boost("tenant_id", 2.0, &schema).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::InvalidFieldSpec
        );
    }

    #[test]
    fn field_boost_rejects_invalid_name() {
        let schema = eng_schema();
        let cfg = Bm25ScoringConfig::new(
            1.2,
            0.75,
            FieldCombinationStrategy::Bm25F,
            LengthNormalizationStrategy::Uniform,
            IdfScope::Global,
        )
        .unwrap();
        let err = cfg.with_field_boost("bad name", 2.0, &schema).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::InvalidFieldName
        );
    }

    #[test]
    fn field_boost_rejects_out_of_range_boost() {
        let schema = eng_schema();
        let cfg = Bm25ScoringConfig::new(
            1.2,
            0.75,
            FieldCombinationStrategy::Bm25F,
            LengthNormalizationStrategy::Uniform,
            IdfScope::Global,
        )
        .unwrap();
        // Boost at the lower boundary (0.0) is valid: FIELD_BOOST_MIN is inclusive.
        assert!(cfg
            .clone()
            .with_field_boost("content", 0.0, &schema)
            .is_ok());
        // Boost above the upper boundary is rejected.
        let err = cfg
            .with_field_boost("content", FIELD_BOOST_MAX + 0.1, &schema)
            .unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::InvalidScoring
        );
    }

    #[test]
    fn debug_redacts_field_names() {
        let schema = eng_schema();
        let cfg = Bm25ScoringConfig::new(
            1.2,
            0.75,
            FieldCombinationStrategy::Bm25F,
            LengthNormalizationStrategy::Uniform,
            IdfScope::Global,
        )
        .unwrap()
        .with_field_boost("content", 3.0, &schema)
        .unwrap();
        let debug = format!("{:?}", cfg);
        assert!(debug.contains("field_boost_count"));
        assert!(!debug.contains("content"));
    }

    #[test]
    fn tenant_strictness_rejects_global_scope() {
        let cfg = Bm25ScoringConfig::new(
            1.2,
            0.75,
            FieldCombinationStrategy::Bm25F,
            LengthNormalizationStrategy::Uniform,
            IdfScope::Global,
        )
        .unwrap();
        let err = cfg.validate_tenant_strictness(true).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::IncompatibleIdfScope
        );
    }

    #[test]
    fn tenant_strictness_accepts_per_tenant() {
        let cfg = Bm25ScoringConfig::new(
            1.2,
            0.75,
            FieldCombinationStrategy::Bm25F,
            LengthNormalizationStrategy::Uniform,
            IdfScope::PerTenant,
        )
        .unwrap();
        assert!(cfg.validate_tenant_strictness(true).is_ok());
    }

    #[test]
    fn combination_strategy_classification() {
        assert!(FieldCombinationStrategy::Bm25F.is_bm25f());
        assert!(!FieldCombinationStrategy::WeightedRrf.is_bm25f());
        assert!(FieldCombinationStrategy::WeightedSum.is_post_scoring_fusion());
        assert!(FieldCombinationStrategy::WeightedRrf.is_post_scoring_fusion());
        assert!(!FieldCombinationStrategy::Bm25F.is_post_scoring_fusion());
    }

    #[test]
    fn idf_scope_tenant_isolation() {
        assert!(IdfScope::PerTenant.is_tenant_isolated());
        assert!(IdfScope::PerCollection.is_tenant_isolated());
        assert!(!IdfScope::Global.is_tenant_isolated());
        assert!(!IdfScope::Segmented.is_tenant_isolated());
    }
}
