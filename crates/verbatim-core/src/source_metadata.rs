//! Typed source/evidence metadata contract with provenance (META-001 / issue #336).
//!
//! This module is the first walking skeleton for queryable metadata: typed field
//! values, origin, confidence, extractor identity, observation time, scope, and
//! deterministic precedence so lower-trust origins (especially model-derived)
//! cannot silently override higher-trust values or weaken ACL/lifecycle/rights.
//!
//! Residual (not in this slice): ingest/parser wiring, DSL/facet export surfaces,
//! storage/index invalidation hooks, full Markdown-thread adapter, migrations of
//! existing untyped JSON, and closing epic #336. See
//! `docs/architecture/source-metadata.md`.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Schema version for [`SourceMetadata`] and related wire forms.
///
/// Unknown versions must fail closed on decode rather than being silently
/// accepted as current-schema entries.
pub const SOURCE_METADATA_SCHEMA_VERSION: u32 = 1;

/// Well-known metadata field names used by query, lifecycle, and ingest.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataFieldName {
    Title,
    Language,
    Author,
    Account,
    PublishedAt,
    ModifiedAt,
    OriginUrl,
    ThreadId,
    Tags,
    Mime,
    Jurisdiction,
    ProductVersion,
    Lifecycle,
    Classification,
    Rights,
    Acl,
    /// Namespaced custom field; wire key is the full `namespace.key` string.
    Custom(String),
}

impl MetadataFieldName {
    /// Stable wire key for maps and precedence tables.
    pub fn wire_key(&self) -> String {
        match self {
            Self::Title => "title".into(),
            Self::Language => "language".into(),
            Self::Author => "author".into(),
            Self::Account => "account".into(),
            Self::PublishedAt => "published_at".into(),
            Self::ModifiedAt => "modified_at".into(),
            Self::OriginUrl => "origin_url".into(),
            Self::ThreadId => "thread_id".into(),
            Self::Tags => "tags".into(),
            Self::Mime => "mime".into(),
            Self::Jurisdiction => "jurisdiction".into(),
            Self::ProductVersion => "product_version".into(),
            Self::Lifecycle => "lifecycle".into(),
            Self::Classification => "classification".into(),
            Self::Rights => "rights".into(),
            Self::Acl => "acl".into(),
            Self::Custom(key) => key.clone(),
        }
    }

    /// Whether this field participates in ACL / lifecycle / rights protection.
    ///
    /// Model-derived (and other low-trust) candidates cannot weaken these fields.
    pub fn is_protected(&self) -> bool {
        matches!(
            self,
            Self::Lifecycle | Self::Classification | Self::Rights | Self::Acl
        )
    }
}

/// Origin of a metadata observation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataOrigin {
    /// Native metadata embedded by the source format or protocol.
    SourceNative,
    /// Explicit front matter (YAML/TOML/etc.).
    FrontMatter,
    /// Deterministic parser extraction from body content.
    Parser,
    /// Filesystem path, mtime, size, or similar OS state.
    Filesystem,
    /// Explicit user override.
    User,
    /// Deterministic rule (e.g. MIME from extension table, not model guess).
    DeterministicRule,
    /// Model-derived guess (lowest trust; never authoritative for protected fields).
    ModelDerived,
}

impl MetadataOrigin {
    /// Base trust rank: higher wins when field-specific tables do not apply.
    ///
    /// Rank is intentionally sparse so field tables can insert intermediate
    /// priorities without renumbering.
    pub fn base_trust_rank(self) -> u8 {
        match self {
            Self::User => 100,
            Self::SourceNative => 80,
            Self::FrontMatter => 70,
            Self::DeterministicRule => 60,
            Self::Parser => 50,
            Self::Filesystem => 30,
            Self::ModelDerived => 10,
        }
    }

    /// True for origins that must never authoritatively set protected fields.
    pub fn is_low_trust(self) -> bool {
        matches!(self, Self::ModelDerived | Self::Filesystem)
    }
}

/// Scope of a metadata observation relative to storage hierarchy.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataScope {
    Source,
    Snapshot,
    Evidence,
    Collection,
}

/// Logical value type for a metadata field.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataValueType {
    Text,
    LanguageTag,
    DateTime,
    Url,
    StringList,
    MimeType,
    LifecycleState,
    ClassificationLabel,
    ProductVersion,
    Custom,
}

/// Typed metadata payload.
///
/// Invalid/ambiguous values for strict query surfaces should be rejected at the
/// adapter boundary; this enum only carries well-formed candidates.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", content = "data", rename_all = "snake_case")]
pub enum MetadataValue {
    Text(String),
    LanguageTag(String),
    /// RFC 3339 timestamp when known; timezone must be preserved in the string.
    DateTime(String),
    Url(String),
    StringList(Vec<String>),
    MimeType(String),
    LifecycleState(String),
    ClassificationLabel(String),
    ProductVersion(String),
    Custom(String),
}

impl MetadataValue {
    /// Corresponding logical type.
    pub fn value_type(&self) -> MetadataValueType {
        match self {
            Self::Text(_) => MetadataValueType::Text,
            Self::LanguageTag(_) => MetadataValueType::LanguageTag,
            Self::DateTime(_) => MetadataValueType::DateTime,
            Self::Url(_) => MetadataValueType::Url,
            Self::StringList(_) => MetadataValueType::StringList,
            Self::MimeType(_) => MetadataValueType::MimeType,
            Self::LifecycleState(_) => MetadataValueType::LifecycleState,
            Self::ClassificationLabel(_) => MetadataValueType::ClassificationLabel,
            Self::ProductVersion(_) => MetadataValueType::ProductVersion,
            Self::Custom(_) => MetadataValueType::Custom,
        }
    }

    /// True when a datetime string is missing or blank (not a parse error).
    pub fn is_missing_datetime(&self) -> bool {
        match self {
            Self::DateTime(s) => s.trim().is_empty(),
            _ => false,
        }
    }
}

/// Confidence band for an observation (not a calibrated probability).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MetadataConfidence {
    High,
    Medium,
    Low,
    /// Explicit "hint only" band — must not be treated as gold/authoritative.
    HintOnly,
}

/// Where a value came from and why it is (or was) selected.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MetadataProvenance {
    pub origin: MetadataOrigin,
    /// Extractor, adapter, or override identity (e.g. `parser:yaml-fm@1`).
    pub extractor_id: String,
    /// Observation time as Unix seconds (UTC).
    pub observed_at_unix: u64,
    /// Human/machine reason the candidate won or was retained.
    pub reason: String,
    /// Optional predecessor retained for audit when policy permits.
    pub superseded_by: Option<String>,
}

impl MetadataProvenance {
    /// Construct provenance for a newly selected (winning) value.
    pub fn selected(
        origin: MetadataOrigin,
        extractor_id: impl Into<String>,
        observed_at_unix: u64,
        reason: impl Into<String>,
    ) -> Self {
        Self {
            origin,
            extractor_id: extractor_id.into(),
            observed_at_unix,
            reason: reason.into(),
            superseded_by: None,
        }
    }
}

/// Construction inputs for [`SourceMetadataField::new`].
#[derive(Debug, Clone)]
pub struct SourceMetadataFieldParams {
    pub name: MetadataFieldName,
    pub value: MetadataValue,
    pub origin: MetadataOrigin,
    pub confidence: MetadataConfidence,
    pub extractor_id: String,
    /// Observation time as Unix seconds (UTC).
    pub observed_at_unix: u64,
    pub scope: MetadataScope,
    pub reason: String,
}

/// Single typed metadata field observation (winner or retained candidate).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceMetadataField {
    pub name: MetadataFieldName,
    pub value_type: MetadataValueType,
    pub value: MetadataValue,
    pub origin: MetadataOrigin,
    pub confidence: MetadataConfidence,
    pub extractor_id: String,
    /// Observation time as Unix seconds (UTC).
    pub observed_at_unix: u64,
    pub scope: MetadataScope,
    pub provenance: MetadataProvenance,
}

impl SourceMetadataField {
    /// Build a field and keep provenance origin aligned with `origin`.
    pub fn new(params: SourceMetadataFieldParams) -> Result<Self> {
        let value_type = params.value.value_type();
        validate_field_value_type(&params.name, value_type)?;
        Ok(Self {
            name: params.name,
            value_type,
            value: params.value,
            origin: params.origin,
            confidence: params.confidence,
            extractor_id: params.extractor_id.clone(),
            observed_at_unix: params.observed_at_unix,
            scope: params.scope,
            provenance: MetadataProvenance::selected(
                params.origin,
                params.extractor_id,
                params.observed_at_unix,
                params.reason,
            ),
        })
    }

    /// Filename-derived title/path labels are always hint-only, never gold.
    pub fn filename_hint(
        name: MetadataFieldName,
        value: MetadataValue,
        extractor_id: impl Into<String>,
        observed_at_unix: u64,
        scope: MetadataScope,
    ) -> Result<Self> {
        Self::new(SourceMetadataFieldParams {
            name,
            value,
            origin: MetadataOrigin::Filesystem,
            confidence: MetadataConfidence::HintOnly,
            extractor_id: extractor_id.into(),
            observed_at_unix,
            scope,
            reason: "filename is a non-authoritative hint only".into(),
        })
    }

    fn wire_key(&self) -> String {
        self.name.wire_key()
    }
}

/// Collection of typed metadata fields with schema identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct SourceMetadata {
    pub schema_version: u32,
    /// Winning fields keyed by wire name.
    pub fields: BTreeMap<String, SourceMetadataField>,
    /// Optional retained superseded observations (audit trail).
    pub superseded: Vec<SourceMetadataField>,
}

impl SourceMetadata {
    /// Empty document on the current schema version.
    pub fn new() -> Self {
        Self {
            schema_version: SOURCE_METADATA_SCHEMA_VERSION,
            fields: BTreeMap::new(),
            superseded: Vec::new(),
        }
    }

    /// Reject unknown or unsupported schema versions.
    pub fn validate_schema(&self) -> Result<()> {
        validate_schema_version(self.schema_version)
    }

    /// Insert or replace a field using deterministic field-specific precedence.
    ///
    /// Returns whether the candidate became the winner. Lower-trust origins
    /// cannot override higher-trust winners. Model-derived (and other low-trust)
    /// candidates cannot set or weaken protected ACL/lifecycle/rights fields.
    pub fn apply_candidate(&mut self, candidate: SourceMetadataField) -> Result<bool> {
        self.validate_schema()?;
        validate_field_value_type(&candidate.name, candidate.value_type)?;
        if candidate.value_type != candidate.value.value_type() {
            bail!(
                "metadata field {} value_type {:?} does not match value kind {:?}",
                candidate.wire_key(),
                candidate.value_type,
                candidate.value.value_type()
            );
        }

        if candidate.name.is_protected() && candidate.origin.is_low_trust() {
            // Retain for audit but never install as authoritative.
            let mut rejected = candidate;
            rejected.provenance.reason = format!(
                "rejected: low-trust origin {:?} cannot set protected field {}",
                rejected.origin,
                rejected.wire_key()
            );
            self.superseded.push(rejected);
            return Ok(false);
        }

        // Filename / filesystem path labels for title-like fields stay hints.
        if is_filename_only_title_candidate(&candidate) {
            let mut hint = candidate;
            hint.confidence = MetadataConfidence::HintOnly;
            hint.provenance.reason =
                "filename-derived title retained as hint only; not authoritative".into();
            // Hints never become winners over existing non-hint titles, and
            // never install as sole authoritative title when empty either if
            // policy is "hint only" — store under superseded for grouping.
            if let Some(existing) = self.fields.get(&hint.wire_key()) {
                if existing.confidence != MetadataConfidence::HintOnly {
                    self.superseded.push(hint);
                    return Ok(false);
                }
            }
            // No non-hint winner yet: still do not promote filename to winner.
            self.superseded.push(hint);
            return Ok(false);
        }

        let key = candidate.wire_key();
        match self.fields.get(&key) {
            None => {
                self.fields.insert(key, candidate);
                Ok(true)
            }
            Some(existing) => {
                let decision = compare_precedence(existing, &candidate);
                match decision {
                    PrecedenceDecision::KeepExisting => {
                        let mut lost = candidate;
                        lost.provenance.reason = format!(
                            "superseded: existing origin {:?} outranks candidate {:?}",
                            existing.origin, lost.origin
                        );
                        lost.provenance.superseded_by = Some(existing.extractor_id.clone());
                        self.superseded.push(lost);
                        Ok(false)
                    }
                    PrecedenceDecision::ReplaceWithCandidate => {
                        // Protected fields: refuse candidates that weaken lifecycle/ACL.
                        if existing.name.is_protected()
                            && candidate_weakens_protected(existing, &candidate)
                        {
                            let mut lost = candidate;
                            lost.provenance.reason = format!(
                                "rejected: candidate would weaken protected field {}",
                                existing.wire_key()
                            );
                            self.superseded.push(lost);
                            return Ok(false);
                        }
                        let mut old = self.fields.remove(&key).expect("existing key");
                        old.provenance.reason = format!(
                            "superseded by higher/equal-priority candidate from {:?}",
                            candidate.origin
                        );
                        old.provenance.superseded_by = Some(candidate.extractor_id.clone());
                        self.superseded.push(old);
                        self.fields.insert(key, candidate);
                        Ok(true)
                    }
                }
            }
        }
    }

    /// Strict query accessor: returns the winning field or an error if missing/invalid.
    pub fn require_field(&self, name: &MetadataFieldName) -> Result<&SourceMetadataField> {
        self.validate_schema()?;
        let key = name.wire_key();
        let field = self
            .fields
            .get(&key)
            .ok_or_else(|| anyhow::anyhow!("missing required metadata field {key}"))?;
        if field.confidence == MetadataConfidence::HintOnly {
            bail!("metadata field {key} is hint-only and not valid for strict queries");
        }
        if matches!(field.value, MetadataValue::DateTime(_)) && field.value.is_missing_datetime() {
            bail!("metadata field {key} has missing datetime value");
        }
        Ok(field)
    }

    /// Approved-field projection for DSL/facets/exports (excludes hint-only).
    pub fn approved_fields(&self) -> impl Iterator<Item = (&String, &SourceMetadataField)> {
        self.fields
            .iter()
            .filter(|(_, f)| f.confidence != MetadataConfidence::HintOnly)
    }
}

/// Decode JSON [`SourceMetadata`] and reject unknown schema versions.
pub fn decode_source_metadata_json(bytes: &[u8]) -> Result<SourceMetadata> {
    let meta: SourceMetadata = serde_json::from_slice(bytes)?;
    meta.validate_schema()?;
    Ok(meta)
}

/// Field-specific trust rank for an origin (higher wins).
///
/// Title treats filesystem/filename as lowest and never authoritative.
/// Protected lifecycle/ACL/classification prefer user and source-native.
pub fn origin_precedence_rank(field: &MetadataFieldName, origin: MetadataOrigin) -> u8 {
    match field {
        MetadataFieldName::Title => match origin {
            MetadataOrigin::User => 100,
            MetadataOrigin::FrontMatter => 90,
            MetadataOrigin::SourceNative => 80,
            MetadataOrigin::DeterministicRule => 60,
            MetadataOrigin::Parser => 50,
            MetadataOrigin::ModelDerived => 20,
            MetadataOrigin::Filesystem => 5, // filename hint only
        },
        MetadataFieldName::PublishedAt | MetadataFieldName::ModifiedAt => match origin {
            MetadataOrigin::User => 100,
            MetadataOrigin::SourceNative => 90,
            MetadataOrigin::FrontMatter => 80,
            MetadataOrigin::DeterministicRule => 70,
            MetadataOrigin::Parser => 50,
            MetadataOrigin::Filesystem => 40, // mtime is usable but below native
            MetadataOrigin::ModelDerived => 10,
        },
        MetadataFieldName::Lifecycle
        | MetadataFieldName::Classification
        | MetadataFieldName::Rights
        | MetadataFieldName::Acl => match origin {
            MetadataOrigin::User => 100,
            MetadataOrigin::SourceNative => 90,
            MetadataOrigin::FrontMatter => 70,
            MetadataOrigin::DeterministicRule => 60,
            MetadataOrigin::Parser => 40,
            MetadataOrigin::Filesystem => 5,
            MetadataOrigin::ModelDerived => 0, // blocked separately
        },
        MetadataFieldName::ThreadId => match origin {
            MetadataOrigin::User => 100,
            MetadataOrigin::SourceNative => 90,
            MetadataOrigin::FrontMatter => 80,
            MetadataOrigin::DeterministicRule => 70,
            MetadataOrigin::Parser => 50,
            MetadataOrigin::Filesystem => 20,
            MetadataOrigin::ModelDerived => 10,
        },
        _ => origin.base_trust_rank(),
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PrecedenceDecision {
    KeepExisting,
    ReplaceWithCandidate,
}

fn compare_precedence(
    existing: &SourceMetadataField,
    candidate: &SourceMetadataField,
) -> PrecedenceDecision {
    let existing_rank = origin_precedence_rank(&existing.name, existing.origin);
    let candidate_rank = origin_precedence_rank(&candidate.name, candidate.origin);
    if candidate_rank > existing_rank {
        PrecedenceDecision::ReplaceWithCandidate
    } else if candidate_rank < existing_rank {
        PrecedenceDecision::KeepExisting
    } else {
        // Tie-break: higher confidence band, then later observation time, then keep existing.
        let conf_rank = |c: MetadataConfidence| match c {
            MetadataConfidence::High => 4,
            MetadataConfidence::Medium => 3,
            MetadataConfidence::Low => 2,
            MetadataConfidence::HintOnly => 1,
        };
        let cand_conf = conf_rank(candidate.confidence);
        let exist_conf = conf_rank(existing.confidence);
        if cand_conf > exist_conf
            || (cand_conf == exist_conf && candidate.observed_at_unix > existing.observed_at_unix)
        {
            PrecedenceDecision::ReplaceWithCandidate
        } else {
            PrecedenceDecision::KeepExisting
        }
    }
}

fn is_filename_only_title_candidate(field: &SourceMetadataField) -> bool {
    matches!(field.name, MetadataFieldName::Title)
        && matches!(field.origin, MetadataOrigin::Filesystem)
}

/// Protected-field weakening: model/low-trust must not move a restrictive state
/// to a more open one. This skeleton uses a simple lifecycle ladder.
fn candidate_weakens_protected(
    existing: &SourceMetadataField,
    candidate: &SourceMetadataField,
) -> bool {
    if !existing.name.is_protected() {
        return false;
    }
    // Only treat string lifecycle/classification labels as ordered here.
    let restrictiveness = |v: &MetadataValue| -> u8 {
        let label = match v {
            MetadataValue::LifecycleState(s)
            | MetadataValue::ClassificationLabel(s)
            | MetadataValue::Text(s)
            | MetadataValue::Custom(s) => s.as_str(),
            _ => return 0,
        };
        match label.to_ascii_lowercase().as_str() {
            "public" | "open" | "active" => 1,
            "internal" | "restricted" => 2,
            "confidential" | "secret" | "hold" | "legal_hold" | "deleted" => 3,
            _ => 0,
        }
    };
    let existing_r = restrictiveness(&existing.value);
    let candidate_r = restrictiveness(&candidate.value);
    existing_r > 0 && candidate_r > 0 && candidate_r < existing_r
}

fn validate_field_value_type(
    name: &MetadataFieldName,
    value_type: MetadataValueType,
) -> Result<()> {
    let expected = match name {
        MetadataFieldName::Title | MetadataFieldName::Author | MetadataFieldName::Account => {
            MetadataValueType::Text
        }
        MetadataFieldName::Language => MetadataValueType::LanguageTag,
        MetadataFieldName::PublishedAt | MetadataFieldName::ModifiedAt => {
            MetadataValueType::DateTime
        }
        MetadataFieldName::OriginUrl => MetadataValueType::Url,
        MetadataFieldName::ThreadId => MetadataValueType::Text,
        MetadataFieldName::Tags => MetadataValueType::StringList,
        MetadataFieldName::Mime => MetadataValueType::MimeType,
        MetadataFieldName::Jurisdiction | MetadataFieldName::Rights | MetadataFieldName::Acl => {
            MetadataValueType::Text
        }
        MetadataFieldName::ProductVersion => MetadataValueType::ProductVersion,
        MetadataFieldName::Lifecycle => MetadataValueType::LifecycleState,
        MetadataFieldName::Classification => MetadataValueType::ClassificationLabel,
        MetadataFieldName::Custom(_) => MetadataValueType::Custom,
    };
    if value_type != expected {
        bail!(
            "metadata field {} expects value type {:?}, got {:?}",
            name.wire_key(),
            expected,
            value_type
        );
    }
    Ok(())
}

fn validate_schema_version(schema_version: u32) -> Result<()> {
    if schema_version != SOURCE_METADATA_SCHEMA_VERSION {
        bail!(
            "unsupported source metadata schema version {schema_version}; expected {SOURCE_METADATA_SCHEMA_VERSION}"
        );
    }
    Ok(())
}

/// Markdown-thread adapter helper: treat path/filename as a non-authoritative hint.
///
/// Benchmark grouping and title gold must not depend on model-generated filenames.
pub fn markdown_thread_filename_hint(
    filename: &str,
    extractor_id: impl Into<String>,
    observed_at_unix: u64,
    scope: MetadataScope,
) -> Result<SourceMetadataField> {
    let stem = filename
        .rsplit('/')
        .next()
        .unwrap_or(filename)
        .trim_end_matches(".md")
        .trim_end_matches(".markdown");
    SourceMetadataField::filename_hint(
        MetadataFieldName::Title,
        MetadataValue::Text(stem.to_string()),
        extractor_id,
        observed_at_unix,
        scope,
    )
}

#[cfg(test)]
#[path = "source_metadata_tests.rs"]
mod tests;
