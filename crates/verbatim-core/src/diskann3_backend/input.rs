//! Vector payloads bound to an embedding profile and publication generation.

use crate::diskann3::PublicationGeneration;
use crate::types::EmbeddingProfileId;

use super::{
    DiskAnnBackendDiagnosticCode, DiskAnnBackendError, DiskAnnBackendResult, VectorSpaceSpec,
};

/// A full-precision vector carrying the identity required to admit it to an index generation.
#[derive(Clone, PartialEq)]
pub struct VectorInput {
    values: Vec<f32>,
    profile_id: EmbeddingProfileId,
    generation: PublicationGeneration,
}

impl VectorInput {
    /// Binds caller-owned full-precision values to one embedding profile and generation.
    pub fn new(
        values: Vec<f32>,
        profile_id: EmbeddingProfileId,
        generation: PublicationGeneration,
    ) -> Self {
        Self {
            values,
            profile_id,
            generation,
        }
    }

    /// Returns the original `f32` values without allowing mutation after binding.
    pub fn values(&self) -> &[f32] {
        &self.values
    }

    /// Returns the embedding profile that produced this vector.
    pub fn profile_id(&self) -> &EmbeddingProfileId {
        &self.profile_id
    }

    /// Returns the publication generation to which this vector is bound.
    pub const fn generation(&self) -> PublicationGeneration {
        self.generation
    }
}

impl VectorSpaceSpec {
    /// Validates a vector's values, profile identity, and generation at an adapter boundary.
    pub fn validate_input(
        &self,
        input: &VectorInput,
        expected_generation: PublicationGeneration,
    ) -> DiskAnnBackendResult<()> {
        if input.profile_id() != self.profile_id() {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::ProfileMismatch,
            ));
        }
        if input.generation != expected_generation {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::GenerationMismatch,
            ));
        }
        self.validate_vector(&input.values)
    }
}
