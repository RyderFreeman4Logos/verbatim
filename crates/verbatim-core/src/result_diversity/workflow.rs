//! Mode-typed adapter boundary; no live diversity algorithm is supplied here.

use async_trait::async_trait;
use std::marker::PhantomData;

use super::{
    DiversityBudget, DiversityGroup, DiversityProfile, DiversityResult, DiversityStageOutput,
    RawCandidateRanking,
};

mod sealed {
    pub trait Sealed {}
}

/// Type-level application surface. Adapters choose one mode explicitly rather
/// than inferring presentation policy from an untyped string.
pub trait DiversityMode: sealed::Sealed + Send + Sync + 'static {
    const NAME: &'static str;
}

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ExploratorySearch;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PrecisionRetrieve;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ContextPack;
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Exhaustive;

impl sealed::Sealed for ExploratorySearch {}
impl sealed::Sealed for PrecisionRetrieve {}
impl sealed::Sealed for ContextPack {}
impl sealed::Sealed for Exhaustive {}

impl DiversityMode for ExploratorySearch {
    const NAME: &'static str = "exploratory_search";
}
impl DiversityMode for PrecisionRetrieve {
    const NAME: &'static str = "precision_retrieve";
}
impl DiversityMode for ContextPack {
    const NAME: &'static str = "context_pack";
}
impl DiversityMode for Exhaustive {
    const NAME: &'static str = "exhaustive";
}

/// A mode-bound immutable input. Raw ranks and occurrence counts enter by value
/// and can only later be observed through immutable references.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiversityRequest<M: DiversityMode> {
    raw_ranking: RawCandidateRanking,
    profile: DiversityProfile,
    budget: DiversityBudget,
    marker: PhantomData<M>,
}

impl<M: DiversityMode> DiversityRequest<M> {
    pub fn new(
        raw_ranking: RawCandidateRanking,
        profile: DiversityProfile,
        budget: DiversityBudget,
    ) -> Self {
        Self {
            raw_ranking,
            profile,
            budget,
            marker: PhantomData,
        }
    }

    pub fn raw_ranking(&self) -> &RawCandidateRanking {
        &self.raw_ranking
    }

    pub fn profile(&self) -> &DiversityProfile {
        &self.profile
    }

    pub fn budget(&self) -> &DiversityBudget {
        &self.budget
    }
}

/// Adapter contract only: first derive groups, then choose representatives, and
/// finally emit a report which retains every raw rank and collapsed member.
#[async_trait]
pub trait ResultDiversityWorkflow: Send + Sync {
    async fn group<M: DiversityMode>(
        &self,
        request: &DiversityRequest<M>,
    ) -> DiversityResult<Vec<DiversityGroup>>;

    async fn select_representatives<M: DiversityMode>(
        &self,
        request: &DiversityRequest<M>,
        groups: Vec<DiversityGroup>,
    ) -> DiversityResult<Vec<DiversityGroup>>;

    async fn emit_collapse_report<M: DiversityMode>(
        &self,
        request: &DiversityRequest<M>,
        groups: Vec<DiversityGroup>,
    ) -> DiversityResult<DiversityStageOutput>;
}
