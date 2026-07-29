//! Lexical field specifications, schema identity, and analyzer identity for the
//! Tantivy lexical engine (Refs #380).
//!
//! These types define the *complete, versioned* lexical field set, the
//! analyzer identity that tokenizes each field, and the schema version that is
//! bound to a [`super::generation::LexicalGeneration`]. They are pure contract
//! types — there is no live Tantivy schema object, no tokenizer binding, and no
//! index open here.
//!
//! ## Field-name and boost safety
//!
//! Field names are validated to a closed character set and length bound so that
//! a diagnostic never echoes arbitrary caller input. Field boosts are bounded
//! positive finite values. The schema is redacted in `Debug`: only the field
//! count and version are emitted, never the field names themselves.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::error::{LexicalEngineDiagnosticCode, LexicalEngineError, LexicalEngineResult};

/// Maximum number of characters in a lexical field name.
pub const FIELD_NAME_MAX_LEN: usize = 64;

/// Maximum number of fields in a single lexical schema.
pub const SCHEMA_MAX_FIELDS: usize = 256;

/// Lexical field value/storage type.
///
/// Each variant maps to a Tantivy field type family for the future live index,
/// but the contract is defined here independently so the schema can be compared
/// across backends without a Tantivy binding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LexicalFieldType {
    /// Tokenized natural-language text (body, title, heading).
    Text,
    /// Untokenized raw identifier/keyword string (exact match).
    Keyword,
    /// CJK / mixed CJK-Latin analyzed text.
    CjkText,
    /// Raw code symbol, identifier, error code, hash, URL, version, or date.
    Identifier,
    /// Typed metadata used as a filter/facet (string facet value).
    Facet,
    /// I64 fast field (e.g. lifecycle epoch, generation id).
    I64,
    /// U64 fast field (e.g. tenant id, source id, collection id).
    U64,
    /// Bool fast field (e.g. ACL flag, is_active).
    Bool,
    /// F64 fast field (e.g. numeric metadata, score adjustment).
    F64,
    /// Date fast field (e.g. ingest/created/updated timestamps).
    Date,
}

impl LexicalFieldType {
    /// Returns `true` if this type is an indexed tokenized text field that
    /// participates in BM25 length normalization.
    pub const fn is_tokenized_text(self) -> bool {
        matches!(self, Self::Text | Self::CjkText)
    }

    /// Returns `true` if this type is suitable as a Tantivy fast field
    /// (single-valued, columnar, filterable/sortable).
    pub const fn is_fast_field(self) -> bool {
        matches!(
            self,
            Self::I64 | Self::U64 | Self::Bool | Self::F64 | Self::Date
        )
    }

    /// Returns `true` if this type is an exact-match (non-tokenized) field.
    pub const fn is_exact_match(self) -> bool {
        matches!(self, Self::Keyword | Self::Identifier | Self::Facet)
    }
}

/// Whether a field is indexed (queryable), stored (returned in docs), or both.
///
/// These map directly to Tantivy's `INDEXED | STORED` field-introspection
/// flags, but are defined here so the contract is backend-independent.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FieldIndexingFlags {
    indexed: bool,
    stored: bool,
    fast: bool,
}

impl FieldIndexingFlags {
    /// Constructs indexing flags with validation. A field must be at least
    /// indexed or stored; `fast` requires a fast-field-compatible type.
    pub(crate) fn new(
        indexed: bool,
        stored: bool,
        fast: bool,
        field_type: LexicalFieldType,
    ) -> LexicalEngineResult<Self> {
        if !indexed && !stored {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidFieldSpec,
            ));
        }
        if fast && !field_type.is_fast_field() {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidFieldSpec,
            ));
        }
        Ok(Self {
            indexed,
            stored,
            fast,
        })
    }

    /// Indexed text field (queryable, not stored).
    pub const fn indexed() -> Self {
        Self {
            indexed: true,
            stored: false,
            fast: false,
        }
    }

    /// Stored field (returned in docs, not queryable).
    pub const fn stored() -> Self {
        Self {
            indexed: false,
            stored: true,
            fast: false,
        }
    }

    /// Indexed + stored field.
    pub const fn indexed_stored() -> Self {
        Self {
            indexed: true,
            stored: true,
            fast: false,
        }
    }

    /// Fast field (columnar, filterable/sortable; implies indexed).
    pub const fn fast() -> Self {
        Self {
            indexed: true,
            stored: false,
            fast: true,
        }
    }

    pub const fn is_indexed(self) -> bool {
        self.indexed
    }

    pub const fn is_stored(self) -> bool {
        self.stored
    }

    pub const fn is_fast(self) -> bool {
        self.fast
    }
}

/// Specification of a single lexical field.
///
/// The field name is validated to a closed character set and length. The boost
/// is a positive finite value applied during BM25 scoring; only tokenized text
/// fields may carry a boost.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "LexicalFieldSpecWire", into = "LexicalFieldSpecWire")]
pub struct LexicalFieldSpec {
    /// Closed, validated field name.
    name: FieldName,
    /// Field value/storage type.
    field_type: LexicalFieldType,
    /// Indexing/storage/fast flags.
    flags: FieldIndexingFlags,
    /// Optional positive finite BM25 boost (defaults to 1.0).
    boost: Option<f64>,
}

impl LexicalFieldSpec {
    /// Constructs a new field spec.
    pub fn new(
        name: impl Into<String>,
        field_type: LexicalFieldType,
        flags: FieldIndexingFlags,
        boost: Option<f64>,
    ) -> LexicalEngineResult<Self> {
        let name = FieldName::new(name.into())?;
        let flags = FieldIndexingFlags::new(
            flags.is_indexed(),
            flags.is_stored(),
            flags.is_fast(),
            field_type,
        )?;
        if let Some(b) = boost {
            if !b.is_finite() || b <= 0.0 {
                return Err(LexicalEngineError::contract(
                    LexicalEngineDiagnosticCode::InvalidScoring,
                ));
            }
            if !field_type.is_tokenized_text() {
                return Err(LexicalEngineError::contract(
                    LexicalEngineDiagnosticCode::InvalidFieldSpec,
                ));
            }
        }
        Ok(Self {
            name,
            field_type,
            flags,
            boost,
        })
    }

    /// Convenience constructor for a tokenized text field with a boost.
    pub fn text(name: impl Into<String>, boost: Option<f64>) -> LexicalEngineResult<Self> {
        Self::new(
            name,
            LexicalFieldType::Text,
            FieldIndexingFlags::indexed_stored(),
            boost,
        )
    }

    /// Convenience constructor for a CJK/mixed text field with a boost.
    pub fn cjk_text(name: impl Into<String>, boost: Option<f64>) -> LexicalEngineResult<Self> {
        Self::new(
            name,
            LexicalFieldType::CjkText,
            FieldIndexingFlags::indexed_stored(),
            boost,
        )
    }

    /// Convenience constructor for a keyword/exact field.
    pub fn keyword(name: impl Into<String>) -> LexicalEngineResult<Self> {
        Self::new(
            name,
            LexicalFieldType::Keyword,
            FieldIndexingFlags::indexed_stored(),
            None,
        )
    }

    /// Convenience constructor for an identifier/raw field.
    pub fn identifier(name: impl Into<String>) -> LexicalEngineResult<Self> {
        Self::new(
            name,
            LexicalFieldType::Identifier,
            FieldIndexingFlags::indexed_stored(),
            None,
        )
    }

    /// Convenience constructor for a fast field.
    pub fn fast_field(
        name: impl Into<String>,
        field_type: LexicalFieldType,
    ) -> LexicalEngineResult<Self> {
        Self::new(name, field_type, FieldIndexingFlags::fast(), None)
    }

    pub fn name(&self) -> &str {
        self.name.as_str()
    }

    pub const fn field_type(&self) -> LexicalFieldType {
        self.field_type
    }

    pub const fn flags(&self) -> FieldIndexingFlags {
        self.flags
    }

    /// Returns the BM25 boost, defaulting to 1.0.
    pub fn boost(&self) -> f64 {
        self.boost.unwrap_or(1.0)
    }
}

impl fmt::Debug for LexicalFieldSpec {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redacted: never render the field name; emit only type/flags/boost.
        formatter
            .debug_struct("LexicalFieldSpec")
            .field("name", &"<REDACTED>")
            .field("field_type", &self.field_type)
            .field("flags", &self.flags)
            .field("boost", &self.boost)
            .finish()
    }
}

/// Wire representation for serde round-trip, mirroring [`LexicalFieldSpec`] but
/// carrying the raw name string so deserialization re-runs validation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct LexicalFieldSpecWire {
    name: String,
    field_type: LexicalFieldType,
    indexed: bool,
    stored: bool,
    fast: bool,
    boost: Option<f64>,
}

impl From<LexicalFieldSpec> for LexicalFieldSpecWire {
    fn from(spec: LexicalFieldSpec) -> Self {
        Self {
            name: spec.name.into_string(),
            field_type: spec.field_type,
            indexed: spec.flags.is_indexed(),
            stored: spec.flags.is_stored(),
            fast: spec.flags.is_fast(),
            boost: spec.boost,
        }
    }
}

impl TryFrom<LexicalFieldSpecWire> for LexicalFieldSpec {
    type Error = LexicalEngineError;

    fn try_from(wire: LexicalFieldSpecWire) -> Result<Self, Self::Error> {
        let flags = FieldIndexingFlags::new(wire.indexed, wire.stored, wire.fast, wire.field_type)?;
        Self::new(wire.name, wire.field_type, flags, wire.boost)
    }
}

/// Validated lexical field name: non-empty, ASCII-printable, bounded length.
///
/// The name is retained for internal comparison and indexing but is never
/// rendered in `Debug`/`Display` of error variants.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
pub struct FieldName(String);

impl FieldName {
    /// Constructs a validated field name.
    pub fn new(value: String) -> LexicalEngineResult<Self> {
        if value.is_empty() || value.len() > FIELD_NAME_MAX_LEN {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidFieldName,
            ));
        }
        // Closed set: ASCII letters, digits, underscore. Must start with a
        // letter or underscore (identifier-like), matching Tantivy field-name
        // conventions and preventing diagnostic injection.
        let valid = value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphabetic() || b == b'_')
            && value
                .bytes()
                .skip(1)
                .all(|b| b.is_ascii_alphanumeric() || b == b'_');
        if !valid {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidFieldName,
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn into_string(self) -> String {
        self.0
    }
}

impl fmt::Debug for FieldName {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("FieldName(<REDACTED>)")
    }
}

impl<'de> Deserialize<'de> for FieldName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Analyzer/tokenizer identity bound to a lexical generation.
///
/// The analyzer identity is part of the schema and the
/// [`super::generation::LexicalGeneration`]. A change in analyzer identity
/// requires a staged rebuild and atomic cutover — see
/// [`super::migration::AnalyzerChangeDisclosure`].
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct AnalyzerIdentity {
    /// Closed analyzer family label (e.g. `english`, `cjk`, `code`).
    family: AnalyzerFamily,
    /// Closed analyzer variant within the family.
    variant: AnalyzerVariant,
    /// Identity version, bumped when the analyzer pipeline changes in any way
    /// (tokenization, stemming, stop words, position recording, normalization).
    version: u32,
}

impl AnalyzerIdentity {
    /// Constructs an analyzer identity.
    pub fn new(
        family: AnalyzerFamily,
        variant: AnalyzerVariant,
        version: u32,
    ) -> LexicalEngineResult<Self> {
        if version == 0 {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(Self {
            family,
            variant,
            version,
        })
    }

    pub const fn family(&self) -> AnalyzerFamily {
        self.family
    }

    pub const fn variant(&self) -> AnalyzerVariant {
        self.variant
    }

    pub const fn version(&self) -> u32 {
        self.version
    }
}

/// Closed set of analyzer families.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzerFamily {
    /// English / Latin-script natural language.
    English,
    /// Simplified Chinese natural language.
    SimplifiedChinese,
    /// Mixed CJK/Latin content (per-field language detection).
    MixedCjkLatin,
    /// Code/identifier tokenization (symbols, paths, versions, hashes).
    Code,
    /// Raw identifier (no tokenization, exact/keyword).
    Raw,
}

/// Closed set of analyzer variants.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AnalyzerVariant {
    /// Standard tokenizer with stemming and stop words.
    Standard,
    /// Lowercased, no stemming (for identifiers/code).
    Lowercase,
    /// N-gram tokenizer (for partial/fuzzy identifier match).
    Ngram,
    /// CJK segmentation (dictionary or bigram).
    CjkSegmentation,
    /// Raw keyword (single token, no analysis).
    Keyword,
}

/// Versioned lexical schema: the complete field set + analyzer identity.
///
/// The schema is part of the [`super::generation::LexicalGeneration`] and the
/// `RetrievalProfile`. Schema changes require a generation bump and staged
/// rebuild.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(try_from = "LexicalSchemaWire", into = "LexicalSchemaWire")]
pub struct LexicalSchema {
    fields: Vec<LexicalFieldSpec>,
    analyzer: AnalyzerIdentity,
    version: u32,
}

impl LexicalSchema {
    /// Constructs a new lexical schema, enforcing uniqueness and bounds.
    pub fn new(
        fields: Vec<LexicalFieldSpec>,
        analyzer: AnalyzerIdentity,
        version: u32,
    ) -> LexicalEngineResult<Self> {
        if version == 0 {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidIdentity,
            ));
        }
        if fields.is_empty() || fields.len() > SCHEMA_MAX_FIELDS {
            return Err(LexicalEngineError::contract(
                LexicalEngineDiagnosticCode::InvalidBounds,
            ));
        }
        Self::check_unique(&fields)?;
        Ok(Self {
            fields,
            analyzer,
            version,
        })
    }

    fn check_unique(fields: &[LexicalFieldSpec]) -> LexicalEngineResult<()> {
        let mut seen = std::collections::HashSet::with_capacity(fields.len());
        for spec in fields {
            if !seen.insert(spec.name.as_str()) {
                return Err(LexicalEngineError::contract(
                    LexicalEngineDiagnosticCode::DuplicateField,
                ));
            }
        }
        Ok(())
    }

    pub fn fields(&self) -> &[LexicalFieldSpec] {
        &self.fields
    }

    pub const fn analyzer(&self) -> &AnalyzerIdentity {
        &self.analyzer
    }

    pub const fn version(&self) -> u32 {
        self.version
    }

    /// Returns the number of tokenized text fields eligible for BM25 scoring.
    pub fn tokenized_text_field_count(&self) -> usize {
        self.fields
            .iter()
            .filter(|f| f.field_type().is_tokenized_text())
            .count()
    }
}

impl fmt::Debug for LexicalSchema {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Redacted: emit only counts and version, never field names.
        formatter
            .debug_struct("LexicalSchema")
            .field("field_count", &self.fields.len())
            .field("analyzer", &self.analyzer)
            .field("version", &self.version)
            .finish()
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
struct LexicalSchemaWire {
    fields: Vec<LexicalFieldSpecWire>,
    analyzer: AnalyzerIdentity,
    version: u32,
}

impl From<LexicalSchema> for LexicalSchemaWire {
    fn from(schema: LexicalSchema) -> Self {
        Self {
            fields: schema.fields.into_iter().map(Into::into).collect(),
            analyzer: schema.analyzer,
            version: schema.version,
        }
    }
}

impl TryFrom<LexicalSchemaWire> for LexicalSchema {
    type Error = LexicalEngineError;

    fn try_from(wire: LexicalSchemaWire) -> Result<Self, Self::Error> {
        let fields = wire
            .fields
            .into_iter()
            .map(TryFrom::try_from)
            .collect::<Result<Vec<_>, _>>()?;
        Self::new(fields, wire.analyzer, wire.version)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn eng_analyzer() -> AnalyzerIdentity {
        AnalyzerIdentity::new(AnalyzerFamily::English, AnalyzerVariant::Standard, 1).unwrap()
    }

    #[test]
    fn field_name_rejects_empty_and_oversized() {
        assert!(FieldName::new(String::new()).is_err());
        assert!(FieldName::new("x".repeat(FIELD_NAME_MAX_LEN + 1)).is_err());
    }

    #[test]
    fn field_name_rejects_invalid_chars() {
        assert!(FieldName::new("1bad".to_string()).is_err());
        assert!(FieldName::new("bad name".to_string()).is_err());
        assert!(FieldName::new("bad-dash".to_string()).is_err());
    }

    #[test]
    fn field_name_accepts_valid() {
        assert!(FieldName::new("content".to_string()).is_ok());
        assert!(FieldName::new("_private".to_string()).is_ok());
        assert!(FieldName::new("a1_b2".to_string()).is_ok());
    }

    #[test]
    fn field_spec_rejects_non_finite_boost() {
        let err = LexicalFieldSpec::text("content", Some(f64::NAN)).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::InvalidScoring
        );
    }

    #[test]
    fn field_spec_rejects_non_positive_boost() {
        let err = LexicalFieldSpec::text("content", Some(0.0)).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::InvalidScoring
        );
        assert!(LexicalFieldSpec::text("content", Some(-1.0)).is_err());
    }

    #[test]
    fn field_spec_rejects_boost_on_non_text() {
        let err = LexicalFieldSpec::new(
            "id",
            LexicalFieldType::U64,
            FieldIndexingFlags::fast(),
            Some(2.0),
        )
        .unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::InvalidFieldSpec
        );
    }

    #[test]
    fn field_spec_boost_defaults_to_one() {
        let spec = LexicalFieldSpec::text("content", None).unwrap();
        assert!((spec.boost() - 1.0).abs() < f64::EPSILON);
    }

    #[test]
    fn flags_reject_neither_indexed_nor_stored() {
        let err = FieldIndexingFlags::new(false, false, false, LexicalFieldType::Text).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::InvalidFieldSpec
        );
    }

    #[test]
    fn flags_reject_fast_on_non_fast_type() {
        let err = FieldIndexingFlags::new(true, false, true, LexicalFieldType::Text).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::InvalidFieldSpec
        );
    }

    #[test]
    fn schema_rejects_duplicate_field_names() {
        let f1 = LexicalFieldSpec::text("content", None).unwrap();
        let f2 = LexicalFieldSpec::text("content", Some(2.0)).unwrap();
        let err = LexicalSchema::new(vec![f1, f2], eng_analyzer(), 1).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::DuplicateField
        );
    }

    #[test]
    fn schema_rejects_empty() {
        let err = LexicalSchema::new(vec![], eng_analyzer(), 1).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::InvalidBounds
        );
    }

    #[test]
    fn schema_rejects_zero_version() {
        let f = LexicalFieldSpec::text("content", None).unwrap();
        let err = LexicalSchema::new(vec![f], eng_analyzer(), 0).unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::InvalidIdentity
        );
    }

    #[test]
    fn schema_debug_redacts_field_names() {
        let f = LexicalFieldSpec::text("secret_field", None).unwrap();
        let schema = LexicalSchema::new(vec![f], eng_analyzer(), 1).unwrap();
        let debug = format!("{:?}", schema);
        assert!(debug.contains("field_count"));
        assert!(!debug.contains("secret_field"));
    }

    #[test]
    fn analyzer_identity_rejects_zero_version() {
        let err = AnalyzerIdentity::new(AnalyzerFamily::English, AnalyzerVariant::Standard, 0)
            .unwrap_err();
        assert_eq!(
            err.diagnostic_code(),
            LexicalEngineDiagnosticCode::InvalidIdentity
        );
    }

    #[test]
    fn field_type_classification() {
        assert!(LexicalFieldType::Text.is_tokenized_text());
        assert!(LexicalFieldType::CjkText.is_tokenized_text());
        assert!(!LexicalFieldType::Keyword.is_tokenized_text());
        assert!(LexicalFieldType::U64.is_fast_field());
        assert!(LexicalFieldType::I64.is_fast_field());
        assert!(!LexicalFieldType::Text.is_fast_field());
        assert!(LexicalFieldType::Identifier.is_exact_match());
        assert!(LexicalFieldType::Keyword.is_exact_match());
    }
}
