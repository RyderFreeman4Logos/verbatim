//! Bounded late-interaction candidate and exact-rescoring contracts.

use serde::{Deserialize, Serialize};

use super::{NamedVectorSpaceDiagnosticCode, NamedVectorSpaceError, NamedVectorSpaceResult};

/// A contiguous, page-aligned original-vector range on SSD for one object.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct VectorRange {
    start_vector_offset: u64,
    vector_count: u32,
    vectors_per_page: u32,
}

impl<'de> Deserialize<'de> for VectorRange {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            start_vector_offset: u64,
            vector_count: u32,
            vectors_per_page: u32,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.start_vector_offset,
            wire.vector_count,
            wire.vectors_per_page,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl VectorRange {
    pub fn new(
        start_vector_offset: u64,
        vector_count: u32,
        vectors_per_page: u32,
    ) -> NamedVectorSpaceResult<Self> {
        if vector_count == 0
            || vectors_per_page == 0
            || !start_vector_offset.is_multiple_of(u64::from(vectors_per_page))
        {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidLateInteractionLayout,
            ));
        }
        Ok(Self {
            start_vector_offset,
            vector_count,
            vectors_per_page,
        })
    }

    pub const fn start_vector_offset(self) -> u64 {
        self.start_vector_offset
    }
    pub const fn vector_count(self) -> u32 {
        self.vector_count
    }
    pub const fn vectors_per_page(self) -> u32 {
        self.vectors_per_page
    }
}

/// Strict caps for the approximate token/region candidate stage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct LateInteractionCandidateStage {
    maximum_query_token_frontier: u32,
    maximum_object_candidate_pool: u32,
    approximate_candidate_stage: bool,
}

impl<'de> Deserialize<'de> for LateInteractionCandidateStage {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            maximum_query_token_frontier: u32,
            maximum_object_candidate_pool: u32,
            approximate_candidate_stage: bool,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.maximum_query_token_frontier,
            wire.maximum_object_candidate_pool,
            wire.approximate_candidate_stage,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl LateInteractionCandidateStage {
    pub fn new(
        maximum_query_token_frontier: u32,
        maximum_object_candidate_pool: u32,
        approximate_candidate_stage: bool,
    ) -> NamedVectorSpaceResult<Self> {
        if maximum_query_token_frontier == 0 || maximum_object_candidate_pool == 0 {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidLateInteractionLayout,
            ));
        }
        Ok(Self {
            maximum_query_token_frontier,
            maximum_object_candidate_pool,
            approximate_candidate_stage,
        })
    }

    pub const fn maximum_query_token_frontier(self) -> u32 {
        self.maximum_query_token_frontier
    }
    pub const fn maximum_object_candidate_pool(self) -> u32 {
        self.maximum_object_candidate_pool
    }
    pub const fn approximate_candidate_stage(self) -> bool {
        self.approximate_candidate_stage
    }
}

/// Declared second-stage interaction. It never upgrades approximate candidate
/// recall into an exact first-stage claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExactInteraction {
    MaxSimFullPrecision,
}

impl ExactInteraction {
    pub const fn max_sim_full_precision() -> Self {
        Self::MaxSimFullPrecision
    }
    pub const fn requires_original_vectors(self) -> bool {
        true
    }

    /// Scores original native-dimensional vectors with ColBERT-style MaxSim.
    ///
    /// Every query token contributes its largest dot product against the
    /// candidate's original vectors. Candidate codes and quantized vectors are
    /// intentionally not accepted by this pure final-scoring contract.
    pub fn score_original_vectors(
        self,
        query_tokens: &[Vec<f32>],
        candidate_tokens: &[Vec<f32>],
    ) -> NamedVectorSpaceResult<f32> {
        let _ = self;
        validate_original_vectors(query_tokens, candidate_tokens)?;
        Ok(query_tokens
            .iter()
            .map(|query_token| {
                candidate_tokens
                    .iter()
                    .map(|candidate_token| {
                        query_token
                            .iter()
                            .zip(candidate_token)
                            .map(|(query, candidate)| query * candidate)
                            .sum::<f32>()
                    })
                    .fold(f32::NEG_INFINITY, f32::max)
            })
            .sum())
    }
}

fn validate_original_vectors(
    query_tokens: &[Vec<f32>],
    candidate_tokens: &[Vec<f32>],
) -> NamedVectorSpaceResult<()> {
    let Some(native_dimension) = query_tokens.first().map(Vec::len) else {
        return Err(NamedVectorSpaceError::contract(
            NamedVectorSpaceDiagnosticCode::InvalidExactInteractionVectors,
        ));
    };
    let valid = native_dimension > 0
        && !candidate_tokens.is_empty()
        && query_tokens.iter().chain(candidate_tokens).all(|vector| {
            vector.len() == native_dimension && vector.iter().all(|value| value.is_finite())
        });
    if valid {
        Ok(())
    } else {
        Err(NamedVectorSpaceError::contract(
            NamedVectorSpaceDiagnosticCode::InvalidExactInteractionVectors,
        ))
    }
}

/// Separate evaluation measurements for candidate recall and final interaction
/// quality. The fields are intentionally not conflated into a single "exact" bit.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LateInteractionQualityMeasurements {
    candidate_recall: f64,
    final_interaction_quality: f64,
    evaluated_query_count: u64,
}

impl<'de> Deserialize<'de> for LateInteractionQualityMeasurements {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            candidate_recall: f64,
            final_interaction_quality: f64,
            evaluated_query_count: u64,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.candidate_recall,
            wire.final_interaction_quality,
            wire.evaluated_query_count,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl LateInteractionQualityMeasurements {
    pub fn new(
        candidate_recall: f64,
        final_interaction_quality: f64,
        evaluated_query_count: u64,
    ) -> NamedVectorSpaceResult<Self> {
        let valid = candidate_recall.is_finite()
            && final_interaction_quality.is_finite()
            && (0.0..=1.0).contains(&candidate_recall)
            && (0.0..=1.0).contains(&final_interaction_quality)
            && evaluated_query_count > 0;
        if !valid {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidLateInteractionMeasurement,
            ));
        }
        Ok(Self {
            candidate_recall,
            final_interaction_quality,
            evaluated_query_count,
        })
    }

    pub const fn candidate_recall(self) -> f64 {
        self.candidate_recall
    }
    pub const fn final_interaction_quality(self) -> f64 {
        self.final_interaction_quality
    }
    pub const fn evaluated_query_count(self) -> u64 {
        self.evaluated_query_count
    }
}
