//! Vector search trait and stateless fail-closed policy helpers.

use super::{
    BoundedCandidates, FilterPredicate, PublicationGeneration, RetrievalStageBudget, SearchBudget,
    VectorDimension, VectorSearchDiagnosticCode, VectorSearchError, VectorSearchResult,
};

/// Pure adapter boundary: candidate generation, rescore, and hydration remain bounded.
pub trait VectorSearchContract {
    fn search(
        &self,
        query: &[f32],
        budget: &SearchBudget,
        filters: &[FilterPredicate],
    ) -> VectorSearchResult<BoundedCandidates>;

    fn rescore(
        &self,
        candidates: BoundedCandidates,
        budget: &SearchBudget,
    ) -> VectorSearchResult<BoundedCandidates>;

    fn hydrate(
        &self,
        candidates: BoundedCandidates,
        budget: &SearchBudget,
    ) -> VectorSearchResult<BoundedCandidates>;
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
    candidates: &BoundedCandidates,
) -> VectorSearchResult<String> {
    let permissive_budget = RetrievalStageBudget::uniform(u32::MAX)?;
    candidates.validate(&permissive_budget)?;
    serde_json::to_string(candidates)
        .map_err(|_| VectorSearchError::contract(VectorSearchDiagnosticCode::SerializationFailed))
}

pub fn decode_bounded_candidates_json(
    input: &str,
    budget: &RetrievalStageBudget,
) -> VectorSearchResult<BoundedCandidates> {
    let candidates: BoundedCandidates = serde_json::from_str(input)
        .map_err(|_| VectorSearchError::contract(VectorSearchDiagnosticCode::InvalidCandidates))?;
    candidates
        .validate(budget)
        .map_err(|_| VectorSearchError::contract(VectorSearchDiagnosticCode::InvalidCandidates))?;
    Ok(candidates)
}
