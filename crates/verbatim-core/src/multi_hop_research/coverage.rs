//! Coverage evaluation: covered / partial / missing facts, conflicts.

use serde::{Deserialize, Serialize};

use super::error::{ResearchError, ResearchResult};
use super::util::require_non_empty;

/// Coverage status for one required fact or relation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CoverageStatus {
    /// Fully covered by explicit evidence paths.
    Covered,
    /// Partially covered; not sufficient for complete status alone.
    Partial,
    /// Missing / unresolved.
    Missing,
    /// Conflict among sources.
    Conflict,
}

impl CoverageStatus {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Covered => "covered",
            Self::Partial => "partial",
            Self::Missing => "missing",
            Self::Conflict => "conflict",
        }
    }

    pub fn is_fully_covered(self) -> bool {
        matches!(self, Self::Covered)
    }
}

/// One required fact's coverage entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FactCoverage {
    pub fact: String,
    pub status: CoverageStatus,
    /// Evidence unit ids supporting this fact (may be empty when Missing).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_unit_ids: Vec<String>,
    /// Subquestion ids that contributed to this fact.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subquestion_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl FactCoverage {
    pub fn validate(&self) -> ResearchResult<()> {
        require_non_empty("fact", &self.fact)?;
        for id in &self.evidence_unit_ids {
            require_non_empty("evidence_unit_id", id)?;
        }
        for id in &self.subquestion_ids {
            require_non_empty("subquestion_id", id)?;
        }
        if let Some(n) = &self.note {
            require_non_empty("note", n)?;
        }
        if self.status.is_fully_covered() && self.evidence_unit_ids.is_empty() {
            return Err(ResearchError::validation(
                "covered fact requires at least one evidence_unit_id",
            ));
        }
        Ok(())
    }
}

/// One required relation's coverage entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RelationCoverage {
    pub relation: String,
    pub status: CoverageStatus,
    /// Edge / path evidence unit ids (graph edges require backing evidence).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub evidence_unit_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub subquestion_ids: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub note: Option<String>,
}

impl RelationCoverage {
    pub fn validate(&self) -> ResearchResult<()> {
        require_non_empty("relation", &self.relation)?;
        for id in &self.evidence_unit_ids {
            require_non_empty("evidence_unit_id", id)?;
        }
        for id in &self.subquestion_ids {
            require_non_empty("subquestion_id", id)?;
        }
        if let Some(n) = &self.note {
            require_non_empty("note", n)?;
        }
        if self.status.is_fully_covered() && self.evidence_unit_ids.is_empty() {
            return Err(ResearchError::validation(
                "covered relation requires at least one evidence_unit_id",
            ));
        }
        Ok(())
    }
}

/// Explicit conflict between evidence units / sources.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageConflict {
    pub conflict_id: String,
    pub summary: String,
    pub evidence_unit_ids: Vec<String>,
}

impl CoverageConflict {
    pub fn validate(&self) -> ResearchResult<()> {
        require_non_empty("conflict_id", &self.conflict_id)?;
        require_non_empty("conflict.summary", &self.summary)?;
        if self.evidence_unit_ids.len() < 2 {
            return Err(ResearchError::validation(
                "coverage conflict requires at least two evidence_unit_ids",
            ));
        }
        for id in &self.evidence_unit_ids {
            require_non_empty("evidence_unit_id", id)?;
        }
        Ok(())
    }
}

/// Aggregate coverage report after a retrieval round.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageReport {
    pub report_id: String,
    /// Round index this report evaluates (1-based).
    pub round_index: u32,
    pub facts: Vec<FactCoverage>,
    pub relations: Vec<RelationCoverage>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub conflicts: Vec<CoverageConflict>,
    /// Unresolved requirement labels (facts/relations still open).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub unresolved_requirements: Vec<String>,
    /// True when all required facts/relations are Covered and no conflicts.
    pub is_complete: bool,
}

/// Field bundle for [`CoverageReport::new`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageReportFields {
    pub report_id: String,
    pub round_index: u32,
    pub facts: Vec<FactCoverage>,
    pub relations: Vec<RelationCoverage>,
    pub conflicts: Vec<CoverageConflict>,
}

impl CoverageReport {
    pub fn new(fields: CoverageReportFields) -> ResearchResult<Self> {
        let unresolved = compute_unresolved(&fields.facts, &fields.relations, &fields.conflicts);
        let is_complete = unresolved.is_empty() && fields.conflicts.is_empty() && {
            let has_requirement = !fields.facts.is_empty() || !fields.relations.is_empty();
            has_requirement
                && fields.facts.iter().all(|f| f.status.is_fully_covered())
                && fields.relations.iter().all(|r| r.status.is_fully_covered())
        };
        let report = Self {
            report_id: fields.report_id,
            round_index: fields.round_index,
            facts: fields.facts,
            relations: fields.relations,
            conflicts: fields.conflicts,
            unresolved_requirements: unresolved,
            is_complete,
        };
        report.validate()?;
        Ok(report)
    }

    pub fn validate(&self) -> ResearchResult<()> {
        require_non_empty("report_id", &self.report_id)?;
        if self.round_index == 0 {
            return Err(ResearchError::validation("round_index must be >= 1"));
        }
        for f in &self.facts {
            f.validate()?;
        }
        for r in &self.relations {
            r.validate()?;
        }
        for c in &self.conflicts {
            c.validate()?;
        }
        for u in &self.unresolved_requirements {
            require_non_empty("unresolved_requirement", u)?;
        }
        let expected_unresolved = compute_unresolved(&self.facts, &self.relations, &self.conflicts);
        if self.unresolved_requirements != expected_unresolved {
            return Err(ResearchError::validation(
                "unresolved_requirements does not match facts/relations/conflicts",
            ));
        }
        let expected_complete = expected_unresolved.is_empty()
            && self.conflicts.is_empty()
            && (!self.facts.is_empty() || !self.relations.is_empty())
            && self.facts.iter().all(|f| f.status.is_fully_covered())
            && self.relations.iter().all(|r| r.status.is_fully_covered());
        if self.is_complete != expected_complete {
            return Err(ResearchError::validation(
                "is_complete flag does not match coverage entries",
            ));
        }
        Ok(())
    }

    /// Whether a corrective round is warranted (incomplete without claiming done).
    pub fn needs_corrective_round(&self) -> bool {
        !self.is_complete
    }
}

fn compute_unresolved(
    facts: &[FactCoverage],
    relations: &[RelationCoverage],
    conflicts: &[CoverageConflict],
) -> Vec<String> {
    let mut out = Vec::new();
    for f in facts {
        if !f.status.is_fully_covered() {
            out.push(f.fact.clone());
        }
    }
    for r in relations {
        if !r.status.is_fully_covered() {
            out.push(r.relation.clone());
        }
    }
    // Conflicts always block complete status; surface conflict ids as unresolved.
    for c in conflicts {
        out.push(format!("conflict:{}", c.conflict_id));
    }
    out.sort();
    out.dedup();
    out
}
