//! Inspectable fusion-stage output retaining all raw provenance.
//!
//! The only durable result of this contract: the fused candidates carry full
//! per-retriever provenance (raw ranks/scores), while the complete retriever
//! results remain embedded for audit. JSON decode revalidates profile,
//! candidate, provenance, and usage invariants.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{
    CompletenessState, FusionBudget, FusionCandidate, FusionDiagnosticCode, FusionError,
    FusionProfile, FusionResult, FusionUsage, RetrieverResult,
};

/// The durable output of a bounded fusion run.
///
/// Candidates are the merged pool; `retriever_results` retains every
/// contributing retriever's raw ranked output so raw ranks/scores survive to
/// debug/evaluation artifacts. Usage is checked against the budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FusionStageOutput {
    profile: FusionProfile,
    retriever_results: Vec<RetrieverResult>,
    candidates: Vec<FusionCandidate>,
    completeness: CompletenessState,
    usage: FusionUsage,
}

impl FusionStageOutput {
    /// Builds an output only after validating profile, retriever results,
    /// candidates, provenance attribution, and budget usage.
    pub fn new(
        profile: FusionProfile,
        retriever_results: Vec<RetrieverResult>,
        candidates: Vec<FusionCandidate>,
        completeness: CompletenessState,
        budget: &FusionBudget,
    ) -> FusionResult<Self> {
        profile.validate()?;
        if retriever_results.is_empty() {
            return Err(FusionError::validation(
                FusionDiagnosticCode::StageOutputRequiresRetrieverResults,
            ));
        }
        for result in &retriever_results {
            result.validate()?;
        }
        if candidates.is_empty() {
            return Err(FusionError::validation(
                FusionDiagnosticCode::StageOutputRequiresCandidates,
            ));
        }
        for candidate in &candidates {
            candidate.validate()?;
        }
        // Every candidate's contributing retriever must exist among the
        // retriever results; every candidate hit id must exist in at least one
        // retriever result.
        let retriever_ids: BTreeSet<&str> = retriever_results
            .iter()
            .map(RetrieverResult::retriever_id)
            .collect();
        let mut candidate_hit_ids: BTreeSet<&str> = BTreeSet::new();
        for candidate in &candidates {
            if !candidate_hit_ids.insert(candidate.hit_id()) {
                return Err(FusionError::validation(
                    FusionDiagnosticCode::StageOutputDuplicateCandidateHitId,
                ));
            }
            for entry in candidate.provenance() {
                if !retriever_ids.contains(entry.retriever_id()) {
                    return Err(FusionError::validation(
                        FusionDiagnosticCode::StageOutputProvenanceRetrieverAbsent,
                    ));
                }
            }
        }
        // Each candidate hit id must appear in at least one retriever result.
        let all_retriever_hit_ids: BTreeSet<&str> = retriever_results
            .iter()
            .flat_map(|r| r.candidates().iter().map(|c| c.hit_id()))
            .collect();
        for hit_id in &candidate_hit_ids {
            if !all_retriever_hit_ids.contains(hit_id) {
                return Err(FusionError::validation(
                    FusionDiagnosticCode::StageOutputCandidateHitIdAbsentFromRetrievers,
                ));
            }
        }
        // Completeness consistency: if any contributing retriever is
        // approximate, the overall state may not be ExactScopeEnumerated
        // unless an exhaustive retriever is also present and covers the scope.
        let any_approximate = retriever_results.iter().any(|r| r.kind().is_approximate());
        completeness.validate_against_approximate(
            any_approximate && { !retriever_results.iter().any(|r| r.may_claim_exhaustive()) },
        )?;
        let usage = FusionUsage {
            retriever_candidates: retriever_results
                .iter()
                .map(|r| r.candidates().len() as u32)
                .sum(),
            fused_pool: candidates.len() as u32,
            rerank_input: 0,
            final_hydration_list: 0,
            debug_output: 0,
        };
        usage.check(budget)?;
        Ok(Self {
            profile,
            retriever_results,
            candidates,
            completeness,
            usage,
        })
    }

    pub fn validate(&self) -> FusionResult<()> {
        self.profile.validate()?;
        if self.retriever_results.is_empty() {
            return Err(FusionError::validation(
                FusionDiagnosticCode::StageOutputRequiresRetrieverResults,
            ));
        }
        for result in &self.retriever_results {
            result.validate()?;
        }
        if self.candidates.is_empty() {
            return Err(FusionError::validation(
                FusionDiagnosticCode::StageOutputRequiresCandidates,
            ));
        }
        for candidate in &self.candidates {
            candidate.validate()?;
        }
        let retriever_ids: BTreeSet<&str> = self
            .retriever_results
            .iter()
            .map(RetrieverResult::retriever_id)
            .collect();
        let mut candidate_hit_ids: BTreeSet<&str> = BTreeSet::new();
        for candidate in &self.candidates {
            if !candidate_hit_ids.insert(candidate.hit_id()) {
                return Err(FusionError::validation(
                    FusionDiagnosticCode::StageOutputDuplicateCandidateHitId,
                ));
            }
            for entry in candidate.provenance() {
                if !retriever_ids.contains(entry.retriever_id()) {
                    return Err(FusionError::validation(
                        FusionDiagnosticCode::StageOutputProvenanceRetrieverAbsent,
                    ));
                }
            }
        }
        let all_retriever_hit_ids: BTreeSet<&str> = self
            .retriever_results
            .iter()
            .flat_map(|r| r.candidates().iter().map(|c| c.hit_id()))
            .collect();
        for hit_id in &candidate_hit_ids {
            if !all_retriever_hit_ids.contains(hit_id) {
                return Err(FusionError::validation(
                    FusionDiagnosticCode::StageOutputCandidateHitIdAbsentFromRetrievers,
                ));
            }
        }
        Ok(())
    }

    pub fn profile(&self) -> &FusionProfile {
        &self.profile
    }

    pub fn retriever_results(&self) -> &[RetrieverResult] {
        &self.retriever_results
    }

    pub fn candidates(&self) -> &[FusionCandidate] {
        &self.candidates
    }

    pub fn completeness(&self) -> &CompletenessState {
        &self.completeness
    }

    pub const fn usage(&self) -> FusionUsage {
        self.usage
    }
}

/// Decode a persisted output only after validating all invariants.
pub fn decode_fusion_stage_output_json(input: &str) -> FusionResult<FusionStageOutput> {
    let output: FusionStageOutput = serde_json::from_str(input)
        .map_err(|_| FusionError::validation(FusionDiagnosticCode::InvalidStageOutputJson))?;
    output.validate()?;
    Ok(output)
}

/// Encode only an output that still satisfies its audit invariants.
pub fn encode_fusion_stage_output_json(output: &FusionStageOutput) -> FusionResult<String> {
    output.validate()?;
    serde_json::to_string(output)
        .map_err(|_| FusionError::validation(FusionDiagnosticCode::StageOutputSerializationFailed))
}
