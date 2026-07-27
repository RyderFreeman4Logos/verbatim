//! Vector search trait and stateless fail-closed policy helpers.

use super::retrieval::decode_hydrated_candidates;
use super::{
    FilterPredicate, FilteredCandidates, FusedCandidates, GeneratedCandidates, HydratedCandidates,
    PublicationGeneration, RerankedCandidates, RescoredCandidates, RetrievalStageBudget,
    SearchBudget, VectorDimension, VectorSearchDiagnosticCode, VectorSearchError,
    VectorSearchResult,
};

/// Pure adapter boundary with a mandatory, typed retrieval pipeline.
pub trait VectorSearchContract {
    fn search(
        &self,
        query: &[f32],
        budget: &SearchBudget,
        filters: &[FilterPredicate],
    ) -> VectorSearchResult<GeneratedCandidates>;

    fn rescore(
        &self,
        candidates: GeneratedCandidates,
        budget: &SearchBudget,
    ) -> VectorSearchResult<RescoredCandidates>;

    fn apply_filters(
        &self,
        candidates: RescoredCandidates,
        budget: &SearchBudget,
    ) -> VectorSearchResult<FilteredCandidates>;

    fn fuse(
        &self,
        candidates: FilteredCandidates,
        budget: &SearchBudget,
    ) -> VectorSearchResult<FusedCandidates>;

    fn rerank(
        &self,
        candidates: FusedCandidates,
        budget: &SearchBudget,
    ) -> VectorSearchResult<RerankedCandidates>;

    fn hydrate(
        &self,
        candidates: RerankedCandidates,
        budget: &SearchBudget,
    ) -> VectorSearchResult<HydratedCandidates>;
}

/// Stateless enforcement helpers that adapters call at every DiskANN3 boundary.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VectorSearchPolicy;

impl VectorSearchPolicy {
    pub const MAX_FILTER_PREDICATES: usize = 16;

    pub fn validate_search(
        &self,
        query: &[f32],
        budget: &SearchBudget,
        filters: &[FilterPredicate],
    ) -> VectorSearchResult<()> {
        VectorDimension::validate_vector(query)?;
        budget.validate()?;
        if filters.len() > Self::MAX_FILTER_PREDICATES {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::FilterUnsupported,
            ));
        }
        for filter in filters {
            filter.validate()?;
        }
        Ok(())
    }

    /// Rejects mixed generation reads during publication and rollback.
    pub fn validate_generation(
        &self,
        expected: PublicationGeneration,
        observed: PublicationGeneration,
    ) -> VectorSearchResult<()> {
        if expected != observed {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::GenerationMismatch,
            ));
        }
        Ok(())
    }

    /// Strict filters must narrow before traversal rather than overfetch the corpus.
    pub fn validate_filter_selectivity(
        &self,
        corpus_count: u64,
        candidate_count: u64,
        strict_filter: bool,
    ) -> VectorSearchResult<()> {
        if candidate_count > corpus_count || (strict_filter && candidate_count >= corpus_count) {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::FilterUnsupported,
            ));
        }
        Ok(())
    }
}

pub fn encode_search_budget_json(budget: &SearchBudget) -> VectorSearchResult<String> {
    budget.validate()?;
    serde_json::to_string(budget)
        .map_err(|_| VectorSearchError::contract(VectorSearchDiagnosticCode::SerializationFailed))
}

pub fn decode_search_budget_json(input: &str) -> VectorSearchResult<SearchBudget> {
    let budget: SearchBudget = serde_json::from_str(input).map_err(|_| {
        VectorSearchError::contract(VectorSearchDiagnosticCode::InvalidSearchBudget)
    })?;
    budget.validate().map_err(|_| {
        VectorSearchError::contract(VectorSearchDiagnosticCode::InvalidSearchBudget)
    })?;
    Ok(budget)
}

pub fn encode_bounded_candidates_json(
    candidates: &HydratedCandidates,
) -> VectorSearchResult<String> {
    let permissive_budget = RetrievalStageBudget::uniform(u32::MAX)?;
    candidates.validate(&permissive_budget)?;
    serde_json::to_string(candidates)
        .map_err(|_| VectorSearchError::contract(VectorSearchDiagnosticCode::SerializationFailed))
}

pub fn decode_bounded_candidates_json(
    input: &str,
    budget: &RetrievalStageBudget,
) -> VectorSearchResult<HydratedCandidates> {
    decode_hydrated_candidates(input, budget)
}
