//! Inspectable fusion-stage output retaining all raw provenance.
//!
//! The only durable result of this contract: the fused candidates carry full
//! per-retriever provenance (raw ranks/scores), while the complete retriever
//! results remain embedded for audit. JSON decode revalidates profile,
//! candidate, provenance, completeness, and usage invariants.

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
/// debug/evaluation artifacts. Usage is checked against the stored budget.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FusionStageOutput {
    profile: FusionProfile,
    retriever_results: Vec<RetrieverResult>,
    candidates: Vec<FusionCandidate>,
    completeness: CompletenessState,
    usage: FusionUsage,
    budget: FusionBudget,
}

impl FusionStageOutput {
    /// Builds an output only after validating profile, retriever results,
    /// candidates, provenance attribution, completeness, and budget usage.
    pub fn new(
        profile: FusionProfile,
        retriever_results: Vec<RetrieverResult>,
        candidates: Vec<FusionCandidate>,
        completeness: CompletenessState,
        budget: &FusionBudget,
    ) -> FusionResult<Self> {
        validate_output_components(&profile, &retriever_results, &candidates, &completeness)?;
        let usage = recompute_usage(&retriever_results, &candidates);
        usage.check(budget)?;
        Ok(Self {
            profile,
            retriever_results,
            candidates,
            completeness,
            usage,
            budget: *budget,
        })
    }

    /// Revalidates every persisted invariant before output is used or encoded.
    pub fn validate(&self) -> FusionResult<()> {
        validate_output_components(
            &self.profile,
            &self.retriever_results,
            &self.candidates,
            &self.completeness,
        )?;
        let usage = recompute_usage(&self.retriever_results, &self.candidates);
        usage.check(&self.budget)?;
        if self.usage != usage {
            return Err(FusionError::validation(
                FusionDiagnosticCode::StageOutputUsageMismatch,
            ));
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

fn validate_output_components(
    profile: &FusionProfile,
    retriever_results: &[RetrieverResult],
    candidates: &[FusionCandidate],
    completeness: &CompletenessState,
) -> FusionResult<()> {
    profile.validate()?;
    if retriever_results.is_empty() {
        return Err(FusionError::validation(
            FusionDiagnosticCode::StageOutputRequiresRetrieverResults,
        ));
    }
    for result in retriever_results {
        result.validate()?;
    }
    // Reject duplicate retriever_id values across the output so provenance
    // binding and scope-contribution checks are unambiguous.
    let mut seen_retriever_ids = BTreeSet::new();
    for result in retriever_results {
        if !seen_retriever_ids.insert(result.retriever_id()) {
            return Err(FusionError::validation(
                FusionDiagnosticCode::StageOutputDuplicateRetrieverId,
            ));
        }
    }
    // Structural revalidation of deserialized completeness coverage.
    validate_completeness_coverage(completeness)?;
    for result in retriever_results {
        validate_completeness_coverage(result.completeness())?;
    }
    if candidates.is_empty() {
        return Err(FusionError::validation(
            FusionDiagnosticCode::StageOutputRequiresCandidates,
        ));
    }

    let mut candidate_hit_ids = BTreeSet::new();
    for candidate in candidates {
        candidate.validate()?;
        if !candidate_hit_ids.insert(candidate.hit_id()) {
            return Err(FusionError::validation(
                FusionDiagnosticCode::StageOutputDuplicateCandidateHitId,
            ));
        }
        validate_provenance_binding(candidate, retriever_results)?;
    }

    let all_retriever_hit_ids: BTreeSet<&str> = retriever_results
        .iter()
        .flat_map(|result| {
            result
                .candidates()
                .iter()
                .map(|candidate| candidate.hit_id())
        })
        .collect();
    for hit_id in candidate_hit_ids {
        if !all_retriever_hit_ids.contains(hit_id) {
            return Err(FusionError::validation(
                FusionDiagnosticCode::StageOutputCandidateHitIdAbsentFromRetrievers,
            ));
        }
    }

    validate_exact_scope_claim(completeness, retriever_results, candidates)
}

fn validate_provenance_binding(
    candidate: &FusionCandidate,
    retriever_results: &[RetrieverResult],
) -> FusionResult<()> {
    for entry in candidate.provenance() {
        let result = retriever_results
            .iter()
            .find(|result| result.retriever_id() == entry.retriever_id())
            .ok_or_else(|| {
                FusionError::validation(FusionDiagnosticCode::StageOutputProvenanceRetrieverAbsent)
            })?;
        let retriever_candidate = result
            .candidates()
            .iter()
            .find(|retriever_candidate| retriever_candidate.hit_id() == candidate.hit_id())
            .ok_or_else(|| {
                FusionError::validation(FusionDiagnosticCode::StageOutputProvenanceMismatch)
            })?;
        if result.kind() != entry.kind()
            || retriever_candidate.raw_rank() != entry.raw_rank()
            || retriever_candidate.raw_score() != entry.raw_score()
        {
            return Err(FusionError::validation(
                FusionDiagnosticCode::StageOutputProvenanceMismatch,
            ));
        }
    }
    Ok(())
}

fn validate_completeness_coverage(completeness: &CompletenessState) -> FusionResult<()> {
    if let CompletenessState::ExactScopeEnumerated { coverage, .. } = completeness {
        if coverage.enumerated() == 0 || coverage.matched() > coverage.enumerated() {
            return Err(FusionError::validation(
                FusionDiagnosticCode::CompletenessCoverageInvalid,
            ));
        }
    }
    Ok(())
}

fn validate_exact_scope_claim(
    completeness: &CompletenessState,
    retriever_results: &[RetrieverResult],
    candidates: &[FusionCandidate],
) -> FusionResult<()> {
    let CompletenessState::ExactScopeEnumerated {
        scope_id,
        coverage: output_coverage,
    } = completeness
    else {
        return Ok(());
    };
    let has_matching_exhaustive_contribution = retriever_results.iter().any(|result| {
        if !result.may_claim_exhaustive() || result.completeness().scope_id() != Some(scope_id) {
            return false;
        }
        // The output's coverage must match the qualifying exhaustive retriever's.
        if let CompletenessState::ExactScopeEnumerated {
            coverage: retriever_coverage,
            ..
        } = result.completeness()
        {
            if retriever_coverage != output_coverage {
                return false;
            }
        }
        candidates.iter().any(|candidate| {
            candidate
                .provenance()
                .iter()
                .any(|entry| entry.retriever_id() == result.retriever_id())
        })
    });
    if !has_matching_exhaustive_contribution {
        return Err(FusionError::completeness_violation(
            completeness.clone(),
            FusionDiagnosticCode::CompletenessApproximateCannotClaimExhaustive,
        ));
    }
    Ok(())
}

fn recompute_usage(
    retriever_results: &[RetrieverResult],
    candidates: &[FusionCandidate],
) -> FusionUsage {
    FusionUsage {
        retriever_candidates: retriever_results
            .iter()
            .map(|result| result.candidates().len() as u32)
            .sum(),
        fused_pool: candidates.len() as u32,
        rerank_input: 0,
        final_hydration_list: 0,
        debug_output: 0,
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
