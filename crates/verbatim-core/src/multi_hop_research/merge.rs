//! Merged ContextPack with subquestion attribution.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::decomposition::SubQuestionId;
use super::error::{ResearchError, ResearchResult};
use super::util::{require_digest, require_non_empty};

/// One evidence unit attributed to one or more subquestions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttributedEvidenceUnit {
    pub evidence_unit_id: String,
    /// Subquestions that selected this unit as direct evidence.
    pub subquestion_ids: Vec<SubQuestionId>,
    /// True when the unit is direct evidence (not expanded/generated only).
    pub is_direct: bool,
    /// Content hash of the source EvidencePack fragment (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub evidence_pack_hash: Option<String>,
}

impl AttributedEvidenceUnit {
    pub fn validate(&self) -> ResearchResult<()> {
        require_non_empty("evidence_unit_id", &self.evidence_unit_id)?;
        if self.subquestion_ids.is_empty() {
            return Err(ResearchError::validation(
                "attributed evidence unit requires at least one subquestion_id",
            ));
        }
        let mut seen = BTreeSet::new();
        for id in &self.subquestion_ids {
            id.validate()?;
            if !seen.insert(id.clone()) {
                return Err(ResearchError::validation(format!(
                    "duplicate subquestion_id {} on evidence unit {}",
                    id.as_str(),
                    self.evidence_unit_id
                )));
            }
        }
        if let Some(h) = &self.evidence_pack_hash {
            require_digest("evidence_pack_hash", h)?;
        }
        Ok(())
    }
}

/// Merged research ContextPack: deduplicated units with subquestion attribution.
///
/// Walking skeleton: does not embed full wire ContextPackEnvelope; binds via
/// optional content hash and preserves ordered selected units for adapters that
/// materialize a wire pack residual.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedContextPack {
    pub pack_id: String,
    /// Content hash of the ResearchQuestion / bound QueryPlan.
    pub research_question_hash: String,
    /// Content hash of the DecompositionPlan used.
    pub decomposition_plan_hash: String,
    /// Ordered attributed evidence units (deduplicated by evidence_unit_id).
    pub units: Vec<AttributedEvidenceUnit>,
    /// Optional content hash of a materialised wire ContextPackEnvelope.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_pack_hash: Option<String>,
    /// Generation / profile bookkeeping (optional).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_ref: Option<String>,
}

/// Field bundle for [`MergedContextPack::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MergedContextPackFields {
    pub pack_id: String,
    pub research_question_hash: String,
    pub decomposition_plan_hash: String,
    pub units: Vec<AttributedEvidenceUnit>,
    pub context_pack_hash: Option<String>,
    pub generation: Option<String>,
    pub profile_ref: Option<String>,
}

impl MergedContextPack {
    pub fn new(fields: MergedContextPackFields) -> ResearchResult<Self> {
        let pack = Self {
            pack_id: fields.pack_id,
            research_question_hash: fields.research_question_hash,
            decomposition_plan_hash: fields.decomposition_plan_hash,
            units: fields.units,
            context_pack_hash: fields.context_pack_hash,
            generation: fields.generation,
            profile_ref: fields.profile_ref,
        };
        pack.validate()?;
        Ok(pack)
    }

    pub fn validate(&self) -> ResearchResult<()> {
        require_non_empty("pack_id", &self.pack_id)?;
        require_digest("research_question_hash", &self.research_question_hash)?;
        require_digest("decomposition_plan_hash", &self.decomposition_plan_hash)?;
        if self.units.is_empty() {
            return Err(ResearchError::validation(
                "merged context pack requires at least one evidence unit",
            ));
        }
        let mut seen = BTreeSet::new();
        for unit in &self.units {
            unit.validate()?;
            if !seen.insert(unit.evidence_unit_id.clone()) {
                return Err(ResearchError::validation(format!(
                    "duplicate evidence_unit_id {} in merged pack",
                    unit.evidence_unit_id
                )));
            }
        }
        if let Some(h) = &self.context_pack_hash {
            require_digest("context_pack_hash", h)?;
        }
        if let Some(g) = &self.generation {
            require_non_empty("generation", g)?;
        }
        if let Some(p) = &self.profile_ref {
            require_non_empty("profile_ref", p)?;
        }
        Ok(())
    }

    /// Ordered unique evidence unit ids (direct and non-direct).
    pub fn selected_unit_ids(&self) -> Vec<&str> {
        self.units
            .iter()
            .map(|u| u.evidence_unit_id.as_str())
            .collect()
    }

    /// Direct evidence unit ids only.
    pub fn direct_unit_ids(&self) -> Vec<&str> {
        self.units
            .iter()
            .filter(|u| u.is_direct)
            .map(|u| u.evidence_unit_id.as_str())
            .collect()
    }
}

/// Pure merge of attributed units: first occurrence wins order; attributes union.
pub fn merge_attributed_units(
    batches: &[Vec<AttributedEvidenceUnit>],
) -> ResearchResult<Vec<AttributedEvidenceUnit>> {
    let mut order: Vec<String> = Vec::new();
    let mut map: std::collections::BTreeMap<String, AttributedEvidenceUnit> =
        std::collections::BTreeMap::new();

    for batch in batches {
        for unit in batch {
            unit.validate()?;
            if let Some(existing) = map.get_mut(&unit.evidence_unit_id) {
                // Union subquestion ids; preserve direct if either is direct.
                for sid in &unit.subquestion_ids {
                    if !existing.subquestion_ids.contains(sid) {
                        existing.subquestion_ids.push(sid.clone());
                    }
                }
                existing.is_direct = existing.is_direct || unit.is_direct;
                if existing.evidence_pack_hash.is_none() {
                    existing.evidence_pack_hash = unit.evidence_pack_hash.clone();
                }
            } else {
                order.push(unit.evidence_unit_id.clone());
                map.insert(unit.evidence_unit_id.clone(), unit.clone());
            }
        }
    }

    let mut out = Vec::with_capacity(order.len());
    for id in order {
        if let Some(u) = map.remove(&id) {
            out.push(u);
        }
    }
    if out.is_empty() {
        return Err(ResearchError::validation(
            "merge_attributed_units produced empty unit list",
        ));
    }
    Ok(out)
}
