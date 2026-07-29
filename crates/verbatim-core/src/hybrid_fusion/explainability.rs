//! Per-candidate explainability reports retaining raw ranks, scores, and
//! the fusion weights applied.
//!
//! Raw ranks/scores and inclusion reasons survive to debug/evaluation
//! artifacts. Normalized scores are recorded alongside, never replacing, the
//! raw values.

use serde::{Deserialize, Serialize};

use super::{
    FusionCandidate, HybridFusionDiagnosticCode, HybridFusionError, HybridFusionResult,
    InclusionReason, RawRank, RawScore, RetrieverKind,
};

/// The normalized score computed for a candidate by one strategy step.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct NormalizedScore {
    value: f64,
}

impl NormalizedScore {
    /// Builds a normalized score, rejecting NaN/infinite values.
    pub fn new(value: f64) -> HybridFusionResult<Self> {
        if !value.is_finite() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::NormalizedScoreNotFinite,
            ));
        }
        Ok(Self { value })
    }

    pub const fn value(self) -> f64 {
        self.value
    }
}

/// One row of the per-candidate explainability report: the retriever, its raw
/// rank and raw score for this candidate, and the normalized score (if any)
/// that the fusion strategy derived from it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExplainabilityRow {
    retriever_id: String,
    kind: RetrieverKind,
    raw_rank: RawRank,
    raw_score: RawScore,
    normalized_score: Option<NormalizedScore>,
}

impl ExplainabilityRow {
    pub fn new(
        retriever_id: String,
        kind: RetrieverKind,
        raw_rank: RawRank,
        raw_score: RawScore,
        normalized_score: Option<NormalizedScore>,
    ) -> HybridFusionResult<Self> {
        if retriever_id.trim().is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::RetrieverIdEmpty,
            ));
        }
        Ok(Self {
            retriever_id,
            kind,
            raw_rank,
            raw_score,
            normalized_score,
        })
    }

    pub fn retriever_id(&self) -> &str {
        &self.retriever_id
    }

    pub const fn kind(&self) -> RetrieverKind {
        self.kind
    }

    pub const fn raw_rank(&self) -> RawRank {
        self.raw_rank
    }

    pub const fn raw_score(&self) -> RawScore {
        self.raw_score
    }

    pub fn normalized_score(&self) -> Option<NormalizedScore> {
        self.normalized_score
    }
}

/// The fusion weight applied for one retriever during this fusion run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AppliedWeight {
    retriever_id: String,
    weight: f64,
}

impl AppliedWeight {
    pub fn new(retriever_id: String, weight: f64) -> HybridFusionResult<Self> {
        if retriever_id.trim().is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::RetrieverIdEmpty,
            ));
        }
        if !weight.is_finite() || weight < 0.0 {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::ProfileWeightsMustBePositive,
            ));
        }
        Ok(Self {
            retriever_id,
            weight,
        })
    }

    pub fn retriever_id(&self) -> &str {
        &self.retriever_id
    }

    pub fn weight(&self) -> f64 {
        self.weight
    }
}

/// Field bag used to construct an [`ExplainabilityReport`].
#[derive(Debug, Clone, PartialEq)]
pub struct ExplainabilityReportFields {
    pub hit_id: String,
    pub inclusion_reason: InclusionReason,
    pub rows: Vec<ExplainabilityRow>,
    pub applied_weights: Vec<AppliedWeight>,
}

/// A per-candidate explainability report. Retains raw ranks/scores from every
/// contributing retriever plus the normalized scores and applied weights.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExplainabilityReport {
    hit_id: String,
    inclusion_reason: InclusionReason,
    rows: Vec<ExplainabilityRow>,
    applied_weights: Vec<AppliedWeight>,
}

impl ExplainabilityReport {
    /// Builds a report, rejecting empty hit id, empty rows, and duplicate
    /// retriever ids across rows.
    pub fn new(fields: ExplainabilityReportFields) -> HybridFusionResult<Self> {
        if fields.hit_id.trim().is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::FusionCandidateHitIdEmpty,
            ));
        }
        if fields.rows.is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::ExplainabilityReportRequiresRows,
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for row in &fields.rows {
            if !seen.insert(row.retriever_id()) {
                return Err(HybridFusionError::validation(
                    HybridFusionDiagnosticCode::ExplainabilityReportDuplicateRetriever,
                ));
            }
        }
        let report = Self {
            hit_id: fields.hit_id,
            inclusion_reason: fields.inclusion_reason,
            rows: fields.rows,
            applied_weights: fields.applied_weights,
        };
        report.validate()?;
        Ok(report)
    }

    pub fn validate(&self) -> HybridFusionResult<()> {
        if self.hit_id.trim().is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::FusionCandidateHitIdEmpty,
            ));
        }
        if self.rows.is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::ExplainabilityReportRequiresRows,
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for row in &self.rows {
            if !seen.insert(row.retriever_id()) {
                return Err(HybridFusionError::validation(
                    HybridFusionDiagnosticCode::ExplainabilityReportDuplicateRetriever,
                ));
            }
        }
        Ok(())
    }

    pub fn hit_id(&self) -> &str {
        &self.hit_id
    }

    pub fn inclusion_reason(&self) -> &InclusionReason {
        &self.inclusion_reason
    }

    pub fn rows(&self) -> &[ExplainabilityRow] {
        &self.rows
    }

    pub fn applied_weights(&self) -> &[AppliedWeight] {
        &self.applied_weights
    }

    /// Builds an explainability report from a fused candidate's provenance.
    /// Each provenance entry becomes a row with its raw rank/score; no
    /// normalized score or applied weight is inferred (callers may attach
    /// those via [`ExplainabilityReportFields`]).
    pub fn from_candidate(
        candidate: &FusionCandidate,
        applied_weights: Vec<AppliedWeight>,
    ) -> HybridFusionResult<Self> {
        let rows = candidate
            .provenance()
            .iter()
            .map(|entry| {
                ExplainabilityRow::new(
                    entry.retriever_id().to_string(),
                    entry.kind(),
                    entry.raw_rank(),
                    entry.raw_score(),
                    None,
                )
            })
            .collect::<HybridFusionResult<Vec<_>>>()?;
        Self::new(ExplainabilityReportFields {
            hit_id: candidate.hit_id().to_string(),
            inclusion_reason: candidate.inclusion_reason().clone(),
            rows,
            applied_weights,
        })
    }
}
