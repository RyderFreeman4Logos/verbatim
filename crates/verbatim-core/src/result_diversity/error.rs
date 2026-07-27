//! Typed, diagnostic-only errors for result-diversity contracts.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::{DiversityBudgetExhaustion, DiversityStage};

pub type DiversityResult<T> = Result<T, DiversityError>;

/// Closed diagnostic codes retain no document text, locators, credentials, or
/// other untrusted context. They are safe to serialize and render in logs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiversityDiagnosticCode {
    RawRankMustBePositive,
    OccurrenceCountMustBePositive,
    RawCandidateHitIdEmpty,
    RawCandidateRankingRequiresCandidates,
    RawCandidateRankingDuplicateHitIds,
    RawCandidateRankingDuplicateRawRanks,
    GroupIdentityMissingProvenance,
    IncompatibleGroupIdentityAndCollapseReason,
    DiversityGroupRequiresRepresentativeAndMembers,
    DiversityGroupDuplicateMembers,
    DiversityGroupMemberAbsent,
    DiversityRepresentativeNotMember,
    DiversityRepresentativeAbsent,
    DiversityRepresentativeWeakerThanMember,
    ProtectedSemanticDistinctionSimilarityCollapse,
    DecodedGroupAttributionInvalid,
    BudgetCapsMustBePositive,
    ProfileSerializationFailed,
    ProfileHashMismatch,
    ProfileVersionInvalid,
    NearDuplicateThresholdInvalid,
    ProfileQuotaInvalid,
    StageOutputRequiresGroup,
    StageOutputMemberAttributionInvalid,
    StageOutputMemberRetentionInvalid,
    DecodedOutputUsageInvalid,
    DecodedOutputDuplicatesRawMember,
    DecodedOutputMemberRetentionInvalid,
    InvalidStageOutputJson,
    StageOutputSerializationFailed,
}

impl DiversityDiagnosticCode {
    const fn as_str(self) -> &'static str {
        match self {
            Self::RawRankMustBePositive => "raw_rank_must_be_positive",
            Self::OccurrenceCountMustBePositive => "occurrence_count_must_be_positive",
            Self::RawCandidateHitIdEmpty => "raw_candidate_hit_id_empty",
            Self::RawCandidateRankingRequiresCandidates => {
                "raw_candidate_ranking_requires_candidates"
            }
            Self::RawCandidateRankingDuplicateHitIds => "raw_candidate_ranking_duplicate_hit_ids",
            Self::RawCandidateRankingDuplicateRawRanks => {
                "raw_candidate_ranking_duplicate_raw_ranks"
            }
            Self::GroupIdentityMissingProvenance => "group_identity_missing_provenance",
            Self::IncompatibleGroupIdentityAndCollapseReason => {
                "incompatible_group_identity_and_collapse_reason"
            }
            Self::DiversityGroupRequiresRepresentativeAndMembers => {
                "diversity_group_requires_representative_and_members"
            }
            Self::DiversityGroupDuplicateMembers => "diversity_group_duplicate_members",
            Self::DiversityGroupMemberAbsent => "diversity_group_member_absent",
            Self::DiversityRepresentativeNotMember => "diversity_representative_not_member",
            Self::DiversityRepresentativeAbsent => "diversity_representative_absent",
            Self::DiversityRepresentativeWeakerThanMember => {
                "diversity_representative_weaker_than_member"
            }
            Self::ProtectedSemanticDistinctionSimilarityCollapse => {
                "protected_semantic_distinction_similarity_collapse"
            }
            Self::DecodedGroupAttributionInvalid => "decoded_group_attribution_invalid",
            Self::BudgetCapsMustBePositive => "budget_caps_must_be_positive",
            Self::ProfileSerializationFailed => "profile_serialization_failed",
            Self::ProfileHashMismatch => "profile_hash_mismatch",
            Self::ProfileVersionInvalid => "profile_version_invalid",
            Self::NearDuplicateThresholdInvalid => "near_duplicate_threshold_invalid",
            Self::ProfileQuotaInvalid => "profile_quota_invalid",
            Self::StageOutputRequiresGroup => "stage_output_requires_group",
            Self::StageOutputMemberAttributionInvalid => "stage_output_member_attribution_invalid",
            Self::StageOutputMemberRetentionInvalid => "stage_output_member_retention_invalid",
            Self::DecodedOutputUsageInvalid => "decoded_output_usage_invalid",
            Self::DecodedOutputDuplicatesRawMember => "decoded_output_duplicates_raw_member",
            Self::DecodedOutputMemberRetentionInvalid => "decoded_output_member_retention_invalid",
            Self::InvalidStageOutputJson => "invalid_stage_output_json",
            Self::StageOutputSerializationFailed => "stage_output_serialization_failed",
        }
    }
}

/// Errors intentionally retain only closed diagnostic codes, never arbitrary
/// document text, embeddings, locators, credentials, or other secret-bearing
/// values.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum DiversityError {
    Validation {
        code: DiversityDiagnosticCode,
    },
    BudgetExhausted {
        exhaustion: DiversityBudgetExhaustion,
    },
    IllegalTransition {
        from: DiversityStage,
        to: DiversityStage,
    },
    Disabled {
        code: DiversityDiagnosticCode,
    },
}

impl DiversityError {
    pub const fn validation(code: DiversityDiagnosticCode) -> Self {
        Self::Validation { code }
    }

    const fn diagnostic_code(&self) -> &'static str {
        match self {
            Self::Validation { code } => code.as_str(),
            Self::BudgetExhausted { .. } => "budget_exhausted",
            Self::IllegalTransition { .. } => "illegal_transition",
            Self::Disabled { code } => code.as_str(),
        }
    }
}

impl fmt::Debug for DiversityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "DiversityError({})", self.diagnostic_code())
    }
}

impl fmt::Display for DiversityError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "result-diversity.{}", self.diagnostic_code())
    }
}

impl Error for DiversityError {}
