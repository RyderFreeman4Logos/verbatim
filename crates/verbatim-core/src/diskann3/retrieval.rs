//! Bounded retrieval-stage outputs and exact-scan planning.

use std::marker::PhantomData;

use serde::{Deserialize, Serialize};

use super::{
    PublicationGeneration, RetrievalStageBudget, VectorSearchDiagnosticCode, VectorSearchError,
    VectorSearchResult,
};

/// Ordered retrieval stages. Every stage has a hard output cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RetrievalStage {
    CandidateGeneration,
    FullPrecisionRescore,
    FilterApplication,
    Fusion,
    Rerank,
    Hydration,
}

impl RetrievalStage {
    pub const ALL: [Self; 6] = [
        Self::CandidateGeneration,
        Self::FullPrecisionRescore,
        Self::FilterApplication,
        Self::Fusion,
        Self::Rerank,
        Self::Hydration,
    ];

    pub const fn index(self) -> usize {
        match self {
            Self::CandidateGeneration => 0,
            Self::FullPrecisionRescore => 1,
            Self::FilterApplication => 2,
            Self::Fusion => 3,
            Self::Rerank => 4,
            Self::Hydration => 5,
        }
    }
}

/// A candidate that survived tombstone and generation checks.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct VectorCandidate {
    pub id: String,
    pub score: f32,
    pub generation: PublicationGeneration,
    tombstoned: bool,
}

impl VectorCandidate {
    pub fn new(
        id: impl Into<String>,
        score: f32,
        generation: PublicationGeneration,
        tombstoned: bool,
    ) -> VectorSearchResult<Self> {
        let candidate = Self {
            id: id.into(),
            score,
            generation,
            tombstoned,
        };
        candidate.validate()?;
        Ok(candidate)
    }

    pub fn validate(&self) -> VectorSearchResult<()> {
        if self.id.is_empty()
            || self.id.len() > 256
            || !self.id.is_ascii()
            || !self.score.is_finite()
        {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::InvalidCandidates,
            ));
        }
        if self.tombstoned {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::TombstonedVector,
            ));
        }
        Ok(())
    }
}

/// Opaque token for candidate-generation output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CandidateGenerationOutput(());
/// Opaque token for full-precision rescore output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FullPrecisionRescoreOutput(());
/// Opaque token for filter-application output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FilterApplicationOutput(());
/// Opaque token for fusion output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FusionOutput(());
/// Opaque token for rerank output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RerankOutput(());
/// Opaque token for hydration output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HydrationOutput(());

#[doc(hidden)]
pub trait RetrievalStageToken {
    const STAGE: RetrievalStage;
}

macro_rules! stage_token {
    ($token:ty, $stage:expr) => {
        impl RetrievalStageToken for $token {
            const STAGE: RetrievalStage = $stage;
        }
    };
}

stage_token!(
    CandidateGenerationOutput,
    RetrievalStage::CandidateGeneration
);
stage_token!(
    FullPrecisionRescoreOutput,
    RetrievalStage::FullPrecisionRescore
);
stage_token!(FilterApplicationOutput, RetrievalStage::FilterApplication);
stage_token!(FusionOutput, RetrievalStage::Fusion);
stage_token!(RerankOutput, RetrievalStage::Rerank);
stage_token!(HydrationOutput, RetrievalStage::Hydration);

/// Candidates with a stage token that prevents skipping required pipeline steps.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BoundedCandidates<S> {
    stage: RetrievalStage,
    generation: PublicationGeneration,
    candidates: Vec<VectorCandidate>,
    #[serde(skip)]
    stage_token: PhantomData<S>,
}

/// Candidate-generation output, which is the only public pipeline entry point.
pub type GeneratedCandidates = BoundedCandidates<CandidateGenerationOutput>;
/// Output after full-precision rescore.
pub type RescoredCandidates = BoundedCandidates<FullPrecisionRescoreOutput>;
/// Output after filters have been applied.
pub type FilteredCandidates = BoundedCandidates<FilterApplicationOutput>;
/// Output after mandatory fusion and RRF truncation.
pub type FusedCandidates = BoundedCandidates<FusionOutput>;
/// Output after reranking.
pub type RerankedCandidates = BoundedCandidates<RerankOutput>;
/// Output after hydration, suitable for serialization and return to callers.
pub type HydratedCandidates = BoundedCandidates<HydrationOutput>;

impl<S: RetrievalStageToken> BoundedCandidates<S> {
    fn from_parts(
        candidates: Vec<VectorCandidate>,
        generation: PublicationGeneration,
        budget: &RetrievalStageBudget,
    ) -> VectorSearchResult<Self> {
        let bounded = Self {
            stage: S::STAGE,
            generation,
            candidates,
            stage_token: PhantomData,
        };
        bounded.validate(budget)?;
        Ok(bounded)
    }

    pub const fn stage(&self) -> RetrievalStage {
        self.stage
    }

    pub const fn generation(&self) -> PublicationGeneration {
        self.generation
    }

    pub fn candidates(&self) -> &[VectorCandidate] {
        &self.candidates
    }

    fn advance<T: RetrievalStageToken>(
        self,
        budget: &RetrievalStageBudget,
    ) -> VectorSearchResult<BoundedCandidates<T>> {
        BoundedCandidates::<T>::from_parts(self.candidates, self.generation, budget)
    }

    pub(crate) fn validate(&self, budget: &RetrievalStageBudget) -> VectorSearchResult<()> {
        budget.validate()?;
        if self.stage != S::STAGE {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::StageOrderInvalid,
            ));
        }
        let cap = usize::try_from(budget.cap(self.stage)).map_err(|_| {
            VectorSearchError::contract(VectorSearchDiagnosticCode::StageOutputExceeded)
        })?;
        if self.candidates.len() > cap {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::StageOutputExceeded,
            ));
        }
        for candidate in &self.candidates {
            candidate.validate()?;
            if candidate.generation != self.generation {
                return Err(VectorSearchError::contract(
                    VectorSearchDiagnosticCode::GenerationMismatch,
                ));
            }
        }
        Ok(())
    }
}

impl BoundedCandidates<CandidateGenerationOutput> {
    pub fn new(
        candidates: Vec<VectorCandidate>,
        generation: PublicationGeneration,
        budget: &RetrievalStageBudget,
    ) -> VectorSearchResult<Self> {
        Self::from_parts(candidates, generation, budget)
    }

    /// Advance only into full-precision rescore after validating its output cap.
    pub fn rescore(self, budget: &RetrievalStageBudget) -> VectorSearchResult<RescoredCandidates> {
        self.advance(budget)
    }
}

impl BoundedCandidates<FullPrecisionRescoreOutput> {
    /// Advance only into filter application after validating its output cap.
    pub fn apply_filters(
        self,
        budget: &RetrievalStageBudget,
    ) -> VectorSearchResult<FilteredCandidates> {
        self.advance(budget)
    }
}

impl BoundedCandidates<FilterApplicationOutput> {
    /// Fusion always truncates to its stage cap before producing an output token.
    pub fn fuse(self, budget: &RetrievalStageBudget) -> VectorSearchResult<FusedCandidates> {
        self.truncate_fusion(budget)
    }

    fn truncate_fusion(
        mut self,
        budget: &RetrievalStageBudget,
    ) -> VectorSearchResult<FusedCandidates> {
        let max = usize::try_from(budget.cap(RetrievalStage::Fusion)).map_err(|_| {
            VectorSearchError::contract(VectorSearchDiagnosticCode::StageOutputExceeded)
        })?;
        self.candidates.truncate(max);
        BoundedCandidates::<FusionOutput>::from_parts(self.candidates, self.generation, budget)
    }
}

impl BoundedCandidates<FusionOutput> {
    /// Advance only into rerank after validating its output cap.
    pub fn rerank(self, budget: &RetrievalStageBudget) -> VectorSearchResult<RerankedCandidates> {
        self.advance(budget)
    }
}

impl BoundedCandidates<RerankOutput> {
    /// Advance only into hydration after validating its output cap.
    pub fn hydrate(self, budget: &RetrievalStageBudget) -> VectorSearchResult<HydratedCandidates> {
        self.advance(budget)
    }
}

#[derive(Deserialize)]
struct SerializedHydratedCandidates {
    stage: RetrievalStage,
    generation: PublicationGeneration,
    candidates: Vec<VectorCandidate>,
}

pub(crate) fn decode_hydrated_candidates(
    input: &str,
    budget: &RetrievalStageBudget,
) -> VectorSearchResult<HydratedCandidates> {
    let serialized: SerializedHydratedCandidates = serde_json::from_str(input)
        .map_err(|_| VectorSearchError::contract(VectorSearchDiagnosticCode::InvalidCandidates))?;
    if serialized.stage != RetrievalStage::Hydration {
        return Err(VectorSearchError::contract(
            VectorSearchDiagnosticCode::StageOrderInvalid,
        ));
    }
    BoundedCandidates::<HydrationOutput>::from_parts(
        serialized.candidates,
        serialized.generation,
        budget,
    )
}

/// Candidate generation selected by strict-filter selectivity.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CandidateGenerationPath {
    AnnTraversal,
    ExactSimdScan,
}

/// Candidate-count boundary at which strict filtering selects exact SIMD scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ExactScanThreshold(u32);

impl ExactScanThreshold {
    pub fn new(value: u32) -> VectorSearchResult<Self> {
        if value == 0 {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::FilterUnsupported,
            ));
        }
        Ok(Self(value))
    }

    pub const fn value(self) -> u32 {
        self.0
    }

    pub const fn choose_path(
        self,
        filtered_candidate_count: u32,
        strict_filter: bool,
    ) -> CandidateGenerationPath {
        if strict_filter && filtered_candidate_count <= self.0 {
            CandidateGenerationPath::ExactSimdScan
        } else {
            CandidateGenerationPath::AnnTraversal
        }
    }
}
