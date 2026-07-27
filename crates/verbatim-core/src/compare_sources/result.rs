//! Structured comparison cells, result, and reusable downstream ContextPack.

use serde::{Deserialize, Serialize};

use crate::wire_schemas::ContextPackEnvelope;

use super::dimension::{ComparisonDimension, DimensionAlignment, DimensionValue};
use super::error::{ComparisonError, ComparisonResultType};
use super::scope::ComparisonScope;
use super::util::{require_digest, require_non_empty};

/// One dimension rendered as two side-bound values and a declared alignment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonCell {
    pub dimension: ComparisonDimension,
    pub left: Option<DimensionValue>,
    pub right: Option<DimensionValue>,
    pub alignment: DimensionAlignment,
    /// Analytical explanation; source quotations remain only on values.
    pub interpretation: Option<String>,
}

impl ComparisonCell {
    pub fn validate_for_scope(&self, scope: &ComparisonScope) -> ComparisonResultType<()> {
        self.dimension.validate()?;
        validate_side_value(
            &self.left,
            &scope.left,
            &self.dimension.dimension_id,
            "left",
        )?;
        validate_side_value(
            &self.right,
            &scope.right,
            &self.dimension.dimension_id,
            "right",
        )?;
        if let Some(interpretation) = &self.interpretation {
            require_non_empty("comparison_cell.interpretation", interpretation)?;
        }
        match self.alignment {
            DimensionAlignment::Agreement
            | DimensionAlignment::Difference
            | DimensionAlignment::Conflict => {
                if self.left.is_none() || self.right.is_none() {
                    return Err(ComparisonError::missing_evidence(
                        "agreement/difference/conflict requires both side values",
                    ));
                }
            }
            DimensionAlignment::Missing => {
                if self.left.is_some() && self.right.is_some() {
                    return Err(ComparisonError::validation(
                        "missing alignment requires at least one absent side value",
                    ));
                }
            }
            DimensionAlignment::Incomparable => {
                if self.left.is_none() || self.right.is_none() {
                    return Err(ComparisonError::missing_evidence(
                        "incomparable alignment still requires both evidenced values",
                    ));
                }
            }
        }
        Ok(())
    }
}

fn validate_side_value(
    value: &Option<DimensionValue>,
    expected: &super::scope::SourceVersion,
    dimension_id: &str,
    side: &str,
) -> ComparisonResultType<()> {
    let Some(value) = value else {
        return Ok(());
    };
    value.validate()?;
    if value.dimension_id != dimension_id
        || value.source_id != expected.source_id
        || value.version_id != expected.version_id
    {
        return Err(ComparisonError::validation(format!(
            "{side} value must bind to its scope side and cell dimension"
        )));
    }
    Ok(())
}

/// Complete structured comparison result. It never collapses quotation text
/// into interpretation-only prose: every non-missing side remains inspectable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonResult {
    pub result_id: String,
    pub scope_hash: String,
    pub cells: Vec<ComparisonCell>,
    pub summary_interpretation: Option<String>,
}

/// Construction fields for [`ComparisonResult`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparisonResultFields {
    pub result_id: String,
    pub scope_hash: String,
    pub cells: Vec<ComparisonCell>,
    pub summary_interpretation: Option<String>,
}

impl ComparisonResult {
    pub fn new(
        fields: ComparisonResultFields,
        scope: &ComparisonScope,
    ) -> ComparisonResultType<Self> {
        let result = Self {
            result_id: fields.result_id,
            scope_hash: fields.scope_hash,
            cells: fields.cells,
            summary_interpretation: fields.summary_interpretation,
        };
        result.validate_for_scope(scope)?;
        Ok(result)
    }

    pub fn validate_for_scope(&self, scope: &ComparisonScope) -> ComparisonResultType<()> {
        scope.require_comparable()?;
        require_non_empty("result_id", &self.result_id)?;
        require_digest("scope_hash", &self.scope_hash)?;
        if self.cells.is_empty() {
            return Err(ComparisonError::missing_evidence(
                "comparison result requires at least one cell",
            ));
        }
        let mut dimensions = std::collections::BTreeSet::new();
        for cell in &self.cells {
            cell.validate_for_scope(scope)?;
            if !dimensions.insert(cell.dimension.dimension_id.as_str()) {
                return Err(ComparisonError::validation(
                    "comparison result must not contain duplicate dimensions",
                ));
            }
        }
        if let Some(summary) = &self.summary_interpretation {
            require_non_empty("summary_interpretation", summary)?;
        }
        Ok(())
    }

    pub fn has_unresolved_cells(&self) -> bool {
        self.cells.iter().any(|cell| {
            matches!(
                cell.alignment,
                DimensionAlignment::Conflict
                    | DimensionAlignment::Missing
                    | DimensionAlignment::Incomparable
            )
        })
    }
}

/// Reusable downstream pack that preserves cells and optionally binds an
/// already-materialized public wire ContextPack envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonContextPack {
    pub pack_id: String,
    pub scope_hash: String,
    pub comparison_result_hash: String,
    pub cells: Vec<ComparisonCell>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub wire_context_pack: Option<ContextPackEnvelope>,
}

/// Construction fields for [`ComparisonContextPack`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparisonContextPackFields {
    pub pack_id: String,
    pub scope_hash: String,
    pub comparison_result_hash: String,
    pub cells: Vec<ComparisonCell>,
    pub wire_context_pack: Option<ContextPackEnvelope>,
}

impl ComparisonContextPack {
    pub fn new(
        fields: ComparisonContextPackFields,
        scope: &ComparisonScope,
    ) -> ComparisonResultType<Self> {
        let pack = Self {
            pack_id: fields.pack_id,
            scope_hash: fields.scope_hash,
            comparison_result_hash: fields.comparison_result_hash,
            cells: fields.cells,
            wire_context_pack: fields.wire_context_pack,
        };
        pack.validate_for_scope(scope)?;
        Ok(pack)
    }

    pub fn validate_for_scope(&self, scope: &ComparisonScope) -> ComparisonResultType<()> {
        require_non_empty("pack_id", &self.pack_id)?;
        require_digest("scope_hash", &self.scope_hash)?;
        require_digest("comparison_result_hash", &self.comparison_result_hash)?;
        if self.cells.is_empty() {
            return Err(ComparisonError::missing_evidence(
                "comparison context pack requires cells",
            ));
        }
        for cell in &self.cells {
            cell.validate_for_scope(scope)?;
        }
        if let Some(wire_pack) = &self.wire_context_pack {
            wire_pack
                .validate()
                .map_err(|err| ComparisonError::validation(format!("wire context pack: {err}")))?;
        }
        Ok(())
    }
}
