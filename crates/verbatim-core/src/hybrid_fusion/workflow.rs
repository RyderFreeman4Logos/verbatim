//! Mode-typed adapter boundary; no live fusion algorithm is supplied here.
//!
//! Adapters choose a mode explicitly rather than inferring presentation policy
//! from an untyped string. The mode markers do not alter raw rank ordering.

use async_trait::async_trait;
use std::marker::PhantomData;

use super::{CompletenessState, FusionBudget, FusionProfile, HybridFusionResult, RetrieverResult};

mod sealed {
    pub trait Sealed {}
}

/// Type-level application surface for a fusion run.
pub trait FusionMode: sealed::Sealed + Send + Sync + 'static {
    const NAME: &'static str;
}

/// Convenience alias used by stage tests and other internal consumers.
pub trait ExploratoryMode: FusionMode {}

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

impl FusionMode for ExploratorySearch {
    const NAME: &'static str = "exploratory_search";
}
impl FusionMode for PrecisionRetrieve {
    const NAME: &'static str = "precision_retrieve";
}
impl FusionMode for ContextPack {
    const NAME: &'static str = "context_pack";
}
impl FusionMode for Exhaustive {
    const NAME: &'static str = "exhaustive";
}

impl ExploratoryMode for ExploratorySearch {}
impl ExploratoryMode for PrecisionRetrieve {}
impl ExploratoryMode for ContextPack {}
impl ExploratoryMode for Exhaustive {}

/// A mode-bound immutable fusion request.
#[derive(Debug, Clone, PartialEq)]
pub struct FusionRequest<M: FusionMode> {
    retriever_results: Vec<RetrieverResult>,
    profile: FusionProfile,
    budget: FusionBudget,
    marker: PhantomData<M>,
}

impl<M: FusionMode> FusionRequest<M> {
    pub fn new(
        retriever_results: Vec<RetrieverResult>,
        profile: FusionProfile,
        budget: FusionBudget,
    ) -> HybridFusionResult<Self> {
        if retriever_results.is_empty() {
            return Err(super::HybridFusionError::validation(
                super::HybridFusionDiagnosticCode::FusionRequestRequiresRetrievers,
            ));
        }
        profile.validate()?;
        Ok(Self {
            retriever_results,
            profile,
            budget,
            marker: PhantomData,
        })
    }

    pub fn retriever_results(&self) -> &[RetrieverResult] {
        &self.retriever_results
    }

    pub fn profile(&self) -> &FusionProfile {
        &self.profile
    }

    pub fn budget(&self) -> &FusionBudget {
        &self.budget
    }
}

/// The terminal result of a fusion run: the fused output plus the overall
/// completeness state derived from the contributing retrievers.
#[derive(Debug, Clone, PartialEq)]
pub struct FusionRunResult {
    output: super::FusionStageOutput,
    completeness: CompletenessState,
}

impl FusionRunResult {
    pub fn new(output: super::FusionStageOutput, completeness: CompletenessState) -> Self {
        Self {
            output,
            completeness,
        }
    }

    pub fn output(&self) -> &super::FusionStageOutput {
        &self.output
    }

    pub fn completeness(&self) -> &CompletenessState {
        &self.completeness
    }
}

/// Adapter contract only: assemble the retriever pool, merge, apply precedence,
/// diversity, rerank, select, and hydrate. Each step is bounded by the request
/// budget. No live retrieval, scoring, or backend binding is supplied here.
#[async_trait]
pub trait HybridFusionWorkflow: Send + Sync {
    async fn assemble_retriever_pool<M: FusionMode>(
        &self,
        request: &FusionRequest<M>,
    ) -> HybridFusionResult<Vec<RetrieverResult>>;

    async fn merge<M: FusionMode>(
        &self,
        request: &FusionRequest<M>,
        retriever_results: Vec<RetrieverResult>,
    ) -> HybridFusionResult<super::FusionStageOutput>;

    async fn run<M: FusionMode>(
        &self,
        request: &FusionRequest<M>,
    ) -> HybridFusionResult<FusionRunResult>;
}
