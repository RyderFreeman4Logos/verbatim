//! Inspectable diversity-stage report retaining all raw members.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{
    DiversityBudget, DiversityError, DiversityGroup, DiversityProfile, DiversityResult,
    DiversityUsage, GroupedMember, RawCandidateRanking,
};

/// The only durable result of this contract: representatives are a projection,
/// while the complete immutable raw ranking remains embedded for audit and
/// exhaustive-occurrence accounting.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DiversityStageOutput {
    profile: DiversityProfile,
    raw_ranking: RawCandidateRanking,
    groups: Vec<DiversityGroup>,
    usage: DiversityUsage,
}

impl DiversityStageOutput {
    pub fn new(
        profile: DiversityProfile,
        raw_ranking: RawCandidateRanking,
        groups: Vec<DiversityGroup>,
        budget: &DiversityBudget,
    ) -> DiversityResult<Self> {
        profile.validate()?;
        raw_ranking.validate()?;
        for group in &groups {
            group.validate_for(&raw_ranking)?;
        }
        if groups.is_empty() {
            return Err(DiversityError::validation(
                "diversity stage output requires at least one group",
            ));
        }
        let mut seen_members = BTreeSet::new();
        for group in &groups {
            for member in group.members() {
                if !seen_members.insert(member.hit_id()) {
                    return Err(DiversityError::validation(
                        "diversity stage output must attribute every raw candidate exactly once",
                    ));
                }
            }
        }
        let expected: BTreeSet<_> = raw_ranking
            .candidates()
            .iter()
            .map(|candidate| candidate.hit_id())
            .collect();
        if seen_members != expected {
            return Err(DiversityError::validation(
                "diversity stage output must retain every raw candidate in a group",
            ));
        }
        let collapsed_members = groups
            .iter()
            .map(|group| group.members().len().saturating_sub(1) as u64)
            .sum();
        let usage = DiversityUsage {
            raw_candidates: raw_ranking.candidates().len() as u64,
            groups: groups.len() as u64,
            collapsed_members,
        };
        usage.check(budget)?;
        Ok(Self {
            profile,
            raw_ranking,
            groups,
            usage,
        })
    }

    pub fn validate(&self) -> DiversityResult<()> {
        self.profile.validate()?;
        self.raw_ranking.validate()?;
        for group in &self.groups {
            group.validate_for(&self.raw_ranking)?;
        }
        let expected_usage = DiversityUsage {
            raw_candidates: self.raw_ranking.candidates().len() as u64,
            groups: self.groups.len() as u64,
            collapsed_members: self
                .groups
                .iter()
                .map(|group| group.members().len().saturating_sub(1) as u64)
                .sum(),
        };
        if self.usage != expected_usage {
            return Err(DiversityError::validation(
                "decoded diversity stage output usage does not match retained members",
            ));
        }
        let mut seen_members = BTreeSet::new();
        for group in &self.groups {
            for member in group.members() {
                if !seen_members.insert(member.hit_id()) {
                    return Err(DiversityError::validation(
                        "decoded diversity stage output duplicates a raw member",
                    ));
                }
            }
        }
        let expected: BTreeSet<_> = self
            .raw_ranking
            .candidates()
            .iter()
            .map(|candidate| candidate.hit_id())
            .collect();
        if seen_members != expected {
            return Err(DiversityError::validation(
                "decoded diversity stage output does not retain every raw candidate",
            ));
        }
        Ok(())
    }

    pub fn profile(&self) -> &DiversityProfile {
        &self.profile
    }

    pub fn raw_ranking(&self) -> &RawCandidateRanking {
        &self.raw_ranking
    }

    pub fn groups(&self) -> &[DiversityGroup] {
        &self.groups
    }

    pub fn usage(&self) -> DiversityUsage {
        self.usage
    }

    /// Find a collapsed member without removing it from its group or raw audit.
    pub fn collapsed_member(&self, hit_id: &str) -> Option<&GroupedMember> {
        self.groups
            .iter()
            .flat_map(DiversityGroup::members)
            .find(|member| member.hit_id() == hit_id && member.collapse_reason().is_some())
    }
}

/// Decode a persisted report only after validating all profile, ranking, group,
/// attribution, and usage invariants. Unknown/malformed values fail closed.
pub fn decode_diversity_stage_output_json(input: &str) -> DiversityResult<DiversityStageOutput> {
    let output: DiversityStageOutput = serde_json::from_str(input)
        .map_err(|_| DiversityError::validation("invalid result-diversity stage output JSON"))?;
    output.validate()?;
    Ok(output)
}

/// Encode only a report that still satisfies its audit invariants.
pub fn encode_diversity_stage_output_json(
    output: &DiversityStageOutput,
) -> DiversityResult<String> {
    output.validate()?;
    serde_json::to_string(output).map_err(|_| {
        DiversityError::validation("result-diversity stage output could not be encoded")
    })
}
