//! O(1)-batch hydration port and complete-result boundary.

use serde::{Deserialize, Serialize};

use super::{
    CandidateValidation, OverfetchError, OverfetchResult, SearchBudget,
    StatementCountInstrumentation,
};

/// Complete result data paired with the lightweight validation that authorized it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FullHydration<T> {
    validation: CandidateValidation,
    value: T,
}

impl<T> FullHydration<T> {
    pub fn new(validation: CandidateValidation, value: T) -> Self {
        Self { validation, value }
    }

    pub fn validation(&self) -> &CandidateValidation {
        &self.validation
    }

    pub fn value(&self) -> &T {
        &self.value
    }

    pub fn into_value(self) -> T {
        self.value
    }
}

/// Completed hydration output that cannot exceed the final-hydration limit.
#[derive(Debug, Clone, PartialEq)]
pub struct HydrationBatch<T> {
    items: Vec<FullHydration<T>>,
}

impl<T> HydrationBatch<T> {
    /// Rejects oversize completed output because truncating after fetching it
    /// would hide unbounded hydration rather than prevent it.
    pub fn new(items: Vec<FullHydration<T>>, budget: &SearchBudget) -> OverfetchResult<Self> {
        budget.validate()?;
        if items.len() > budget.final_hydration_list_size as usize {
            return Err(OverfetchError::UnboundedHydration);
        }
        Ok(Self { items })
    }

    pub fn items(&self) -> &[FullHydration<T>] {
        &self.items
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }
}

/// Authoritative-store boundary for O(1) complete-result hydration batches.
///
/// Each method receives the already bounded candidate slice and statement
/// instrumentation. Implementations must issue one set-oriented batch query per
/// method, record its [`super::HydrationBatchKind`], and never loop over the
/// candidate slice with individual SQL reads.
pub trait BatchHydrationPort {
    type ChunkHeader;
    type ChunkBody;
    type ParentLink;
    type ChunkEvidenceLink;
    type EvidenceUnit;
    type Hydrated;

    fn fetch_chunk_headers(
        &self,
        candidates: &[CandidateValidation],
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Vec<Self::ChunkHeader>>;

    fn fetch_chunk_bodies(
        &self,
        candidates: &[CandidateValidation],
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Vec<Self::ChunkBody>>;

    fn fetch_parent_links(
        &self,
        candidates: &[CandidateValidation],
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Vec<Self::ParentLink>>;

    fn fetch_chunk_evidence_links(
        &self,
        candidates: &[CandidateValidation],
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Vec<Self::ChunkEvidenceLink>>;

    fn fetch_evidence_units(
        &self,
        candidates: &[CandidateValidation],
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Vec<Self::EvidenceUnit>>;

    /// Performs all five set-oriented hydration batches for the bounded input.
    fn hydrate_full_batch(
        &self,
        candidates: &[CandidateValidation],
        statements: &mut StatementCountInstrumentation,
    ) -> OverfetchResult<Vec<FullHydration<Self::Hydrated>>>;
}
