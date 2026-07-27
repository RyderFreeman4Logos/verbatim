//! Dimension definitions, evidence-backed per-side values, and alignment classes.

use serde::{Deserialize, Serialize};

use super::error::{ComparisonError, ComparisonResultType};
use super::util::{require_digest, require_non_empty};

/// A user-meaningful axis on which two source versions are compared.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonDimension {
    pub dimension_id: String,
    pub label: String,
    pub description: Option<String>,
}

/// Construction fields for [`ComparisonDimension`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparisonDimensionFields {
    pub dimension_id: String,
    pub label: String,
    pub description: Option<String>,
}

impl ComparisonDimension {
    pub fn new(fields: ComparisonDimensionFields) -> ComparisonResultType<Self> {
        let dimension = Self {
            dimension_id: fields.dimension_id,
            label: fields.label,
            description: fields.description,
        };
        dimension.validate()?;
        Ok(dimension)
    }

    pub fn validate(&self) -> ComparisonResultType<()> {
        require_non_empty("dimension_id", &self.dimension_id)?;
        require_non_empty("dimension.label", &self.label)?;
        if let Some(description) = &self.description {
            require_non_empty("dimension.description", description)?;
        }
        Ok(())
    }
}

/// Provenance binding an extracted quotation to the selected source version.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EvidenceProvenance {
    pub evidence_unit_id: String,
    pub source_id: String,
    pub version_id: String,
    pub locator: String,
    pub content_hash: String,
}

impl EvidenceProvenance {
    pub fn validate(&self) -> ComparisonResultType<()> {
        require_non_empty("evidence_unit_id", &self.evidence_unit_id)?;
        require_non_empty("provenance.source_id", &self.source_id)?;
        require_non_empty("provenance.version_id", &self.version_id)?;
        require_non_empty("provenance.locator", &self.locator)?;
        require_digest("provenance.content_hash", &self.content_hash)
    }
}

/// Verbatim quotation extracted from one provenance-recorded evidence unit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuotedEvidence {
    pub evidence_unit_id: String,
    pub quotation: String,
}

impl QuotedEvidence {
    pub fn validate(&self) -> ComparisonResultType<()> {
        require_non_empty("quoted_evidence.evidence_unit_id", &self.evidence_unit_id)?;
        require_non_empty("quoted_evidence.quotation", &self.quotation)
    }
}

/// Evidence-backed value for one selected side of a comparison dimension.
///
/// `quotations` are source text. `interpretation` is an explicitly separate
/// analytical statement and is never treated as a quotation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DimensionValue {
    pub dimension_id: String,
    pub source_id: String,
    pub version_id: String,
    pub normalized_value: Option<String>,
    pub quotations: Vec<QuotedEvidence>,
    pub interpretation: Option<String>,
    pub provenance: Vec<EvidenceProvenance>,
}

/// Construction fields for [`DimensionValue`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DimensionValueFields {
    pub dimension_id: String,
    pub source_id: String,
    pub version_id: String,
    pub normalized_value: Option<String>,
    pub quotations: Vec<QuotedEvidence>,
    pub interpretation: Option<String>,
    pub provenance: Vec<EvidenceProvenance>,
}

impl DimensionValue {
    pub fn new(fields: DimensionValueFields) -> ComparisonResultType<Self> {
        let value = Self {
            dimension_id: fields.dimension_id,
            source_id: fields.source_id,
            version_id: fields.version_id,
            normalized_value: fields.normalized_value,
            quotations: fields.quotations,
            interpretation: fields.interpretation,
            provenance: fields.provenance,
        };
        value.validate()?;
        Ok(value)
    }

    pub fn validate(&self) -> ComparisonResultType<()> {
        require_non_empty("dimension_value.dimension_id", &self.dimension_id)?;
        require_non_empty("dimension_value.source_id", &self.source_id)?;
        require_non_empty("dimension_value.version_id", &self.version_id)?;
        if self.quotations.is_empty() || self.provenance.is_empty() {
            return Err(ComparisonError::missing_evidence(
                "dimension value requires quotation and provenance",
            ));
        }
        if let Some(value) = &self.normalized_value {
            require_non_empty("dimension_value.normalized_value", value)?;
        }
        if let Some(interpretation) = &self.interpretation {
            require_non_empty("dimension_value.interpretation", interpretation)?;
        }
        let provenance_ids: std::collections::BTreeSet<_> = self
            .provenance
            .iter()
            .map(|entry| entry.evidence_unit_id.as_str())
            .collect();
        for quotation in &self.quotations {
            quotation.validate()?;
            if !provenance_ids.contains(quotation.evidence_unit_id.as_str()) {
                return Err(ComparisonError::missing_evidence(
                    "quotation must reference provenance evidence_unit_id",
                ));
            }
        }
        for entry in &self.provenance {
            entry.validate()?;
            if entry.source_id != self.source_id || entry.version_id != self.version_id {
                return Err(ComparisonError::validation(
                    "dimension value provenance must match value side identity",
                ));
            }
        }
        Ok(())
    }
}

/// Alignment conclusion for a single structured comparison cell.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DimensionAlignment {
    Agreement,
    Difference,
    Conflict,
    Missing,
    Incomparable,
}

impl DimensionAlignment {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Agreement => "agreement",
            Self::Difference => "difference",
            Self::Conflict => "conflict",
            Self::Missing => "missing",
            Self::Incomparable => "incomparable",
        }
    }
}
