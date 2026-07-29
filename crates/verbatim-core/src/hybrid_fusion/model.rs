//! Retriever results and merged fusion candidates with provenance.
//!
//! Every retriever reports independent rankings, raw scores, filters,
//! generation, and a completeness state. A fused candidate preserves the raw
//! rank and raw score from each contributing retriever so that inclusion
//! reasons survive to debug/evaluation artifacts.

use std::collections::BTreeSet;
use std::num::NonZeroU32;

use serde::{Deserialize, Serialize};

use super::{
    CompletenessState, HybridFusionDiagnosticCode, HybridFusionError, HybridFusionResult,
    RetrieverKind,
};

/// A positive 1-indexed raw rank within a single retriever's output.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RawRank(NonZeroU32);

impl RawRank {
    pub fn new(value: u32) -> HybridFusionResult<Self> {
        NonZeroU32::new(value).map(Self).ok_or_else(|| {
            HybridFusionError::validation(HybridFusionDiagnosticCode::RawRankMustBePositive)
        })
    }

    pub const fn get(self) -> u32 {
        self.0.get()
    }
}

/// The direction in which a raw backend score should be interpreted.
///
/// Dense ANN distance is typically ascending (lower is better); BM25 and
/// exact-reference scores are descending (higher is better). Preserving the
/// direction lets a fusion strategy normalize without losing the raw value.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScoreDirection {
    /// Lower raw score is better (e.g. L2/cosine distance).
    Ascending,
    /// Higher raw score is better (e.g. BM25, exact-reference weight).
    Descending,
}

/// A finite raw backend score or distance, retained verbatim alongside the
/// fused/normalized value.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RawScore {
    value: f64,
    direction: ScoreDirection,
}

impl RawScore {
    /// Builds a raw score, rejecting NaN/infinite values.
    pub fn new(value: f64, direction: ScoreDirection) -> HybridFusionResult<Self> {
        if !value.is_finite() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::RawScoreNotFinite,
            ));
        }
        Ok(Self { value, direction })
    }

    pub const fn value(self) -> f64 {
        self.value
    }

    pub const fn direction(self) -> ScoreDirection {
        self.direction
    }
}

/// Stable identity for an applied predicate/filter. Opaque to the contract.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct FilterIdentity(String);

impl FilterIdentity {
    pub fn new(value: String) -> HybridFusionResult<Self> {
        if value.trim().is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::FilterIdentityEmpty,
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A retriever generation identifier (e.g. DiskANN3 generation, Tantivy
/// commit, exhaustive snapshot cursor). Opaque to the contract.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct RetrieverGeneration(String);

impl RetrieverGeneration {
    pub fn new(value: String) -> HybridFusionResult<Self> {
        if value.trim().is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::RetrieverGenerationInvalid,
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// A single raw candidate emitted by one retriever.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrieverCandidate {
    hit_id: String,
    raw_rank: RawRank,
    raw_score: RawScore,
}

impl RetrieverCandidate {
    pub fn new(hit_id: String, raw_rank: RawRank, raw_score: RawScore) -> HybridFusionResult<Self> {
        if hit_id.trim().is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::FusionCandidateHitIdEmpty,
            ));
        }
        Ok(Self {
            hit_id,
            raw_rank,
            raw_score,
        })
    }

    pub fn hit_id(&self) -> &str {
        &self.hit_id
    }

    pub const fn raw_rank(&self) -> RawRank {
        self.raw_rank
    }

    pub const fn raw_score(&self) -> RawScore {
        self.raw_score
    }
}

/// The bounded ranked candidate stream returned by one retriever.
///
/// Each retriever result carries its kind, generation, applied filter, the
/// ranked candidates, and the completeness state. The kind determines whether
/// an exhaustive completeness claim is permitted.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RetrieverResult {
    retriever_id: String,
    kind: RetrieverKind,
    generation: RetrieverGeneration,
    filter: Option<FilterIdentity>,
    candidates: Vec<RetrieverCandidate>,
    completeness: CompletenessState,
}

impl RetrieverResult {
    /// Builds a retriever result, rejecting empty retriever id, empty/duplicate
    /// candidate hit ids, and completeness claims unsupported by the retriever kind.
    pub fn new(
        retriever_id: String,
        kind: RetrieverKind,
        generation: RetrieverGeneration,
        filter: Option<FilterIdentity>,
        candidates: Vec<RetrieverCandidate>,
        completeness: CompletenessState,
    ) -> HybridFusionResult<Self> {
        if retriever_id.trim().is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::RetrieverIdEmpty,
            ));
        }
        if candidates.is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::RetrieverResultRequiresCandidates,
            ));
        }
        completeness.validate_against_approximate(kind.is_approximate())?;
        if completeness.may_claim_exhaustive() && !kind.may_justify_completeness() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::CompletenessClaimUnsupportedForRetriever,
            ));
        }
        let mut seen = BTreeSet::new();
        for candidate in &candidates {
            if !seen.insert(candidate.hit_id()) {
                return Err(HybridFusionError::validation(
                    HybridFusionDiagnosticCode::RetrieverResultDuplicateHitIds,
                ));
            }
        }
        let result = Self {
            retriever_id,
            kind,
            generation,
            filter,
            candidates,
            completeness,
        };
        result.validate()?;
        Ok(result)
    }

    pub fn validate(&self) -> HybridFusionResult<()> {
        if self.retriever_id.trim().is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::RetrieverIdEmpty,
            ));
        }
        if self.candidates.is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::RetrieverResultRequiresCandidates,
            ));
        }
        self.completeness
            .validate_against_approximate(self.kind.is_approximate())?;
        if self.completeness.may_claim_exhaustive() && !self.kind.may_justify_completeness() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::CompletenessClaimUnsupportedForRetriever,
            ));
        }
        let mut seen = BTreeSet::new();
        for candidate in &self.candidates {
            if !seen.insert(candidate.hit_id()) {
                return Err(HybridFusionError::validation(
                    HybridFusionDiagnosticCode::RetrieverResultDuplicateHitIds,
                ));
            }
        }
        Ok(())
    }

    pub fn retriever_id(&self) -> &str {
        &self.retriever_id
    }

    pub const fn kind(&self) -> RetrieverKind {
        self.kind
    }

    pub fn generation(&self) -> &RetrieverGeneration {
        &self.generation
    }

    pub fn filter(&self) -> Option<&FilterIdentity> {
        self.filter.as_ref()
    }

    pub fn candidates(&self) -> &[RetrieverCandidate] {
        &self.candidates
    }

    pub fn completeness(&self) -> &CompletenessState {
        &self.completeness
    }

    /// Returns `true` when this retriever's completeness state permits an
    /// exhaustive claim over the declared scope.
    pub fn may_claim_exhaustive(&self) -> bool {
        self.completeness.may_claim_exhaustive() && self.kind.may_justify_completeness()
    }
}

/// Why a candidate was included in the fused pool. Preserved verbatim in the
/// explainability report so audit artifacts show the retrieval path.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InclusionReason {
    /// Contributed by the ranked Top-K of one or more retrievers.
    RankedTopK,
    /// Must-include exact reference precedence match.
    ExactReferencePrecedence,
    /// Backend server-side fusion (Qdrant/LanceDB) opt-in contribution.
    BackendServerFusion,
    /// Exhaustive enumeration within a declared scope.
    ExhaustiveScopeMatch,
}

impl InclusionReason {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::RankedTopK => "ranked_top_k",
            Self::ExactReferencePrecedence => "exact_reference_precedence",
            Self::BackendServerFusion => "backend_server_fusion",
            Self::ExhaustiveScopeMatch => "exhaustive_scope_match",
        }
    }

    /// Returns `true` when the reason string is non-empty (always true; used
    /// by validators to satisfy the "non-empty reason" invariant uniformly).
    pub const fn is_present(&self) -> bool {
        true
    }
}

/// The raw rank and score contributed by one retriever for one candidate.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProvenanceEntry {
    retriever_id: String,
    kind: RetrieverKind,
    raw_rank: RawRank,
    raw_score: RawScore,
}

impl ProvenanceEntry {
    pub fn new(
        retriever_id: String,
        kind: RetrieverKind,
        raw_rank: RawRank,
        raw_score: RawScore,
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
}

/// Field bag used to construct a [`FusionCandidate`].
#[derive(Debug, Clone, PartialEq)]
pub struct FusionCandidateFields {
    pub hit_id: String,
    pub provenance: Vec<ProvenanceEntry>,
    pub inclusion_reason: InclusionReason,
}

/// A merged candidate preserving per-retriever provenance: which retrievers
/// contributed, and the raw ranks/scores each one assigned.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FusionCandidate {
    hit_id: String,
    provenance: Vec<ProvenanceEntry>,
    inclusion_reason: InclusionReason,
}

impl FusionCandidate {
    /// Builds a fused candidate, rejecting empty hit id, empty provenance,
    /// missing raw rank/score on any provenance entry, and duplicate
    /// contributing retriever ids.
    pub fn new(fields: FusionCandidateFields) -> HybridFusionResult<Self> {
        if fields.hit_id.trim().is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::FusionCandidateHitIdEmpty,
            ));
        }
        if fields.provenance.is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::FusionCandidateRequiresContributingRetriever,
            ));
        }
        if !fields.inclusion_reason.is_present() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::FusionCandidateInclusionReasonEmpty,
            ));
        }
        let mut seen = BTreeSet::new();
        for entry in &fields.provenance {
            if entry.retriever_id().trim().is_empty() {
                return Err(HybridFusionError::validation(
                    HybridFusionDiagnosticCode::FusionCandidateProvenanceMissingRawRank,
                ));
            }
            if !seen.insert(entry.retriever_id()) {
                return Err(HybridFusionError::validation(
                    HybridFusionDiagnosticCode::FusionCandidateDuplicateContributingRetriever,
                ));
            }
        }
        let candidate = Self {
            hit_id: fields.hit_id,
            provenance: fields.provenance,
            inclusion_reason: fields.inclusion_reason,
        };
        candidate.validate()?;
        Ok(candidate)
    }

    pub fn validate(&self) -> HybridFusionResult<()> {
        if self.hit_id.trim().is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::FusionCandidateHitIdEmpty,
            ));
        }
        if self.provenance.is_empty() {
            return Err(HybridFusionError::validation(
                HybridFusionDiagnosticCode::FusionCandidateRequiresContributingRetriever,
            ));
        }
        let mut seen = BTreeSet::new();
        for entry in &self.provenance {
            if !seen.insert(entry.retriever_id()) {
                return Err(HybridFusionError::validation(
                    HybridFusionDiagnosticCode::FusionCandidateDuplicateContributingRetriever,
                ));
            }
        }
        Ok(())
    }

    pub fn hit_id(&self) -> &str {
        &self.hit_id
    }

    pub fn provenance(&self) -> &[ProvenanceEntry] {
        &self.provenance
    }

    pub fn inclusion_reason(&self) -> &InclusionReason {
        &self.inclusion_reason
    }

    /// Returns the retriever ids that contributed to this candidate.
    pub fn contributing_retriever_ids(&self) -> Vec<&str> {
        self.provenance
            .iter()
            .map(ProvenanceEntry::retriever_id)
            .collect()
    }
}
