//! Immutable raw-ranking and inspectable collapse-group types.

use std::collections::BTreeSet;
use std::num::{NonZeroU32, NonZeroU64};

use serde::{Deserialize, Serialize};

use super::{DiversityError, DiversityResult};

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct RawRank(NonZeroU32);

impl RawRank {
    pub fn new(value: u32) -> DiversityResult<Self> {
        NonZeroU32::new(value)
            .map(Self)
            .ok_or_else(|| DiversityError::validation("raw rank must be positive"))
    }

    pub fn get(self) -> u32 {
        self.0.get()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct OccurrenceCount(NonZeroU64);

impl OccurrenceCount {
    pub fn new(value: u64) -> DiversityResult<Self> {
        NonZeroU64::new(value)
            .map(Self)
            .ok_or_else(|| DiversityError::validation("occurrence count must be positive"))
    }

    pub fn get(self) -> u64 {
        self.0.get()
    }
}

/// Ordered strength preserves direct evidence whenever a group is represented.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum EvidenceStrength {
    Direct,
    Corroborating,
    Thematic,
}

/// A member that must not be merged merely because a similarity score is high.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SemanticDistinction {
    Equivalent,
    LegallyDistinctVersion,
    SemanticallyDistinctTranslation,
}

impl SemanticDistinction {
    fn prohibits_similarity_only_collapse(self) -> bool {
        !matches!(self, Self::Equivalent)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RawCandidateFields {
    pub hit_id: String,
    pub raw_rank: RawRank,
    pub occurrence_count: OccurrenceCount,
    pub evidence_strength: EvidenceStrength,
    pub semantic_distinction: SemanticDistinction,
}

/// A raw candidate has no public mutators: diversity can project it, never
/// rewrite its rank or exhaustive occurrence count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawCandidate {
    hit_id: String,
    raw_rank: RawRank,
    occurrence_count: OccurrenceCount,
    evidence_strength: EvidenceStrength,
    semantic_distinction: SemanticDistinction,
}

impl RawCandidate {
    pub fn new(fields: RawCandidateFields) -> DiversityResult<Self> {
        let candidate = Self {
            hit_id: fields.hit_id,
            raw_rank: fields.raw_rank,
            occurrence_count: fields.occurrence_count,
            evidence_strength: fields.evidence_strength,
            semantic_distinction: fields.semantic_distinction,
        };
        candidate.validate()?;
        Ok(candidate)
    }

    pub fn validate(&self) -> DiversityResult<()> {
        if self.hit_id.trim().is_empty() {
            return Err(DiversityError::validation(
                "raw candidate hit id must not be empty",
            ));
        }
        Ok(())
    }

    pub fn hit_id(&self) -> &str {
        &self.hit_id
    }

    pub fn raw_rank(&self) -> RawRank {
        self.raw_rank
    }

    pub fn occurrence_count(&self) -> OccurrenceCount {
        self.occurrence_count
    }

    pub fn evidence_strength(&self) -> EvidenceStrength {
        self.evidence_strength
    }

    pub fn semantic_distinction(&self) -> SemanticDistinction {
        self.semantic_distinction
    }
}

/// Immutable source ranking retained alongside every diversity projection.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RawCandidateRanking {
    candidates: Vec<RawCandidate>,
}

impl RawCandidateRanking {
    pub fn new(candidates: Vec<RawCandidate>) -> DiversityResult<Self> {
        if candidates.is_empty() {
            return Err(DiversityError::validation(
                "raw candidate ranking requires at least one candidate",
            ));
        }
        let mut ids = BTreeSet::new();
        let mut ranks = BTreeSet::new();
        for candidate in &candidates {
            if !ids.insert(candidate.hit_id()) {
                return Err(DiversityError::validation(
                    "raw candidate ranking must not duplicate hit ids",
                ));
            }
            if !ranks.insert(candidate.raw_rank()) {
                return Err(DiversityError::validation(
                    "raw candidate ranking must not duplicate raw ranks",
                ));
            }
        }
        let ranking = Self { candidates };
        ranking.validate()?;
        Ok(ranking)
    }

    pub fn validate(&self) -> DiversityResult<()> {
        if self.candidates.is_empty() {
            return Err(DiversityError::validation(
                "raw candidate ranking requires at least one candidate",
            ));
        }
        let mut ids = BTreeSet::new();
        let mut ranks = BTreeSet::new();
        for candidate in &self.candidates {
            candidate.validate()?;
            if !ids.insert(candidate.hit_id()) {
                return Err(DiversityError::validation(
                    "raw candidate ranking must not duplicate hit ids",
                ));
            }
            if !ranks.insert(candidate.raw_rank()) {
                return Err(DiversityError::validation(
                    "raw candidate ranking must not duplicate raw ranks",
                ));
            }
        }
        Ok(())
    }

    pub fn candidates(&self) -> &[RawCandidate] {
        &self.candidates
    }

    pub fn candidate(&self, hit_id: &str) -> Option<&RawCandidate> {
        self.candidates
            .iter()
            .find(|candidate| candidate.hit_id() == hit_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum GroupIdentity {
    ExactDuplicate { content_hash: String },
    NearDuplicate { similarity_basis: String },
    Overlap { normalized_span_hash: String },
    ParentChild { parent_hit_id: String },
    Thread { thread_id: String },
    Source { source_id: String },
    Mirror { mirror_family_id: String },
    Version { version_family_id: String },
}

impl GroupIdentity {
    fn validate(&self) -> DiversityResult<()> {
        let identifier = match self {
            Self::ExactDuplicate { content_hash } => content_hash,
            Self::NearDuplicate { similarity_basis } => similarity_basis,
            Self::Overlap {
                normalized_span_hash,
            } => normalized_span_hash,
            Self::ParentChild { parent_hit_id } => parent_hit_id,
            Self::Thread { thread_id } => thread_id,
            Self::Source { source_id } => source_id,
            Self::Mirror { mirror_family_id } => mirror_family_id,
            Self::Version { version_family_id } => version_family_id,
        };
        if identifier.trim().is_empty() {
            return Err(DiversityError::validation(
                "group identity must retain a non-empty provenance key",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollapseReason {
    ExactDuplicate,
    NearDuplicate,
    Overlap,
    ParentChild,
    ThreadQuota,
    SourceQuota,
    Mirror,
    ExplicitEquivalentVersion,
}

impl CollapseReason {
    fn is_similarity_only(&self) -> bool {
        matches!(self, Self::NearDuplicate)
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiversityGroupFields {
    pub identity: GroupIdentity,
    pub representative_hit_id: String,
    pub member_hit_ids: Vec<String>,
    pub collapse_reason: CollapseReason,
}

/// One representative plus every group member and its original raw rank.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroupedMember {
    hit_id: String,
    raw_rank: RawRank,
    collapse_reason: Option<CollapseReason>,
}

impl GroupedMember {
    pub fn hit_id(&self) -> &str {
        &self.hit_id
    }

    pub fn raw_rank(&self) -> RawRank {
        self.raw_rank
    }

    pub fn collapse_reason(&self) -> Option<&CollapseReason> {
        self.collapse_reason.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiversityGroup {
    identity: GroupIdentity,
    representative_hit_id: String,
    collapse_reason: CollapseReason,
    members: Vec<GroupedMember>,
}

impl DiversityGroup {
    pub fn new(
        fields: DiversityGroupFields,
        ranking: &RawCandidateRanking,
    ) -> DiversityResult<Self> {
        fields.identity.validate()?;
        if fields.representative_hit_id.trim().is_empty() || fields.member_hit_ids.is_empty() {
            return Err(DiversityError::validation(
                "diversity group requires representative and members",
            ));
        }
        let mut member_ids = BTreeSet::new();
        let mut resolved = Vec::with_capacity(fields.member_hit_ids.len());
        for hit_id in &fields.member_hit_ids {
            if !member_ids.insert(hit_id) {
                return Err(DiversityError::validation(
                    "diversity group must not duplicate member hit ids",
                ));
            }
            let candidate = ranking.candidate(hit_id).ok_or_else(|| {
                DiversityError::validation("diversity group member is absent from raw ranking")
            })?;
            resolved.push(candidate);
        }
        if !member_ids.contains(&fields.representative_hit_id) {
            return Err(DiversityError::validation(
                "diversity representative must be a raw group member",
            ));
        }
        let representative = ranking
            .candidate(&fields.representative_hit_id)
            .ok_or_else(|| {
                DiversityError::validation("diversity representative is absent from raw ranking")
            })?;
        let strongest = resolved
            .iter()
            .map(|candidate| candidate.evidence_strength())
            .min()
            .ok_or_else(|| DiversityError::validation("diversity group requires members"))?;
        if representative.evidence_strength() != strongest {
            return Err(DiversityError::validation(
                "direct or stronger evidence must be selected before weaker diverse evidence",
            ));
        }
        if fields.collapse_reason.is_similarity_only()
            && resolved.iter().any(|candidate| {
                candidate
                    .semantic_distinction()
                    .prohibits_similarity_only_collapse()
            })
        {
            return Err(DiversityError::validation(
                "legally distinct versions and translations cannot collapse solely by similarity",
            ));
        }
        let members = resolved
            .into_iter()
            .map(|candidate| GroupedMember {
                hit_id: candidate.hit_id().to_owned(),
                raw_rank: candidate.raw_rank(),
                collapse_reason: (candidate.hit_id() != fields.representative_hit_id)
                    .then(|| fields.collapse_reason.clone()),
            })
            .collect();
        Ok(Self {
            identity: fields.identity,
            representative_hit_id: fields.representative_hit_id,
            collapse_reason: fields.collapse_reason,
            members,
        })
    }

    pub fn validate_for(&self, ranking: &RawCandidateRanking) -> DiversityResult<()> {
        let rebuilt = Self::new(
            DiversityGroupFields {
                identity: self.identity.clone(),
                representative_hit_id: self.representative_hit_id.clone(),
                member_hit_ids: self
                    .members
                    .iter()
                    .map(|member| member.hit_id.clone())
                    .collect(),
                collapse_reason: self.collapse_reason.clone(),
            },
            ranking,
        )?;
        if rebuilt != *self {
            return Err(DiversityError::validation(
                "decoded diversity group does not retain canonical raw-member attribution",
            ));
        }
        Ok(())
    }

    pub fn identity(&self) -> &GroupIdentity {
        &self.identity
    }

    pub fn representative_hit_id(&self) -> &str {
        &self.representative_hit_id
    }

    pub fn members(&self) -> &[GroupedMember] {
        &self.members
    }
}
