//! Bounded retrieval-stage outputs and exact-scan planning.

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

/// Candidates carried between stages. Construction rechecks every fail-closed invariant.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BoundedCandidates {
    stage: RetrievalStage,
    generation: PublicationGeneration,
    candidates: Vec<VectorCandidate>,
}

impl BoundedCandidates {
    pub fn new(
        stage: RetrievalStage,
        candidates: Vec<VectorCandidate>,
        generation: PublicationGeneration,
        budget: &RetrievalStageBudget,
    ) -> VectorSearchResult<Self> {
        let bounded = Self {
            stage,
            generation,
            candidates,
        };
        bounded.validate(budget)?;
        Ok(bounded)
    }

    pub fn truncate_fusion(
        mut candidates: Vec<VectorCandidate>,
        generation: PublicationGeneration,
        budget: &RetrievalStageBudget,
    ) -> VectorSearchResult<Self> {
        let max = usize::try_from(budget.cap(RetrievalStage::Fusion)).map_err(|_| {
            VectorSearchError::contract(VectorSearchDiagnosticCode::StageOutputExceeded)
        })?;
        candidates.truncate(max);
        Self::new(RetrievalStage::Fusion, candidates, generation, budget)
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

    pub fn advance_to(
        mut self,
        stage: RetrievalStage,
        budget: &RetrievalStageBudget,
    ) -> VectorSearchResult<Self> {
        if stage < self.stage {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::StageOrderInvalid,
            ));
        }
        self.stage = stage;
        self.validate(budget)?;
        Ok(self)
    }

    pub fn validate(&self, budget: &RetrievalStageBudget) -> VectorSearchResult<()> {
        budget.validate()?;
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

    pub const fn choose_path(self, filtered_candidate_count: u32) -> CandidateGenerationPath {
        if filtered_candidate_count <= self.0 {
            CandidateGenerationPath::ExactSimdScan
        } else {
            CandidateGenerationPath::AnnTraversal
        }
    }
}
