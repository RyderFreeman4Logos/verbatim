//! Validated full-precision vector-space inputs for the adapter boundary.

use crate::diskann3::VectorSpaceId;
use crate::types::EmbeddingProfileId;

use super::{DiskAnnBackendDiagnosticCode, DiskAnnBackendError, DiskAnnBackendResult};

/// Similarity metric whose normalization rule is fixed by [`VectorSpaceSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VectorMetric {
    /// Unit-length vectors compared by cosine similarity.
    Cosine,
    /// Dot-product vectors whose original magnitudes remain meaningful.
    Dot,
    /// Euclidean-distance vectors whose original magnitudes remain meaningful.
    L2,
}

/// Normalization behavior enforced before a vector reaches the adapter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VectorNormalization {
    /// Cosine vectors must already have unit L2 norm.
    UnitL2,
    /// The adapter preserves the original vector magnitude.
    PreserveMagnitude,
}

impl VectorMetric {
    /// Returns the only normalization behavior permitted for this metric.
    pub const fn normalization(self) -> VectorNormalization {
        match self {
            Self::Cosine => VectorNormalization::UnitL2,
            Self::Dot | Self::L2 => VectorNormalization::PreserveMagnitude,
        }
    }
}

/// A 4,096-dimensional vector-space and embedding-profile identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct VectorSpaceSpec {
    vector_space_id: VectorSpaceId,
    profile_id: EmbeddingProfileId,
    metric: VectorMetric,
}

impl VectorSpaceSpec {
    /// The only accepted original-vector dimension.
    pub const DIMENSION: usize = 4_096;
    /// Maximum tolerated absolute error around unit cosine normalization.
    pub const COSINE_UNIT_LENGTH_TOLERANCE: f64 = 1.0e-4;

    pub fn new(
        vector_space_id: VectorSpaceId,
        profile_id: EmbeddingProfileId,
        metric: VectorMetric,
    ) -> DiskAnnBackendResult<Self> {
        Ok(Self {
            vector_space_id,
            profile_id,
            metric,
        })
    }

    /// Returns the fixed full-precision `f32` dimension.
    pub const fn dimension(&self) -> u32 {
        Self::DIMENSION as u32
    }

    pub const fn metric(&self) -> VectorMetric {
        self.metric
    }

    pub fn vector_space_id(&self) -> &VectorSpaceId {
        &self.vector_space_id
    }

    pub fn profile_id(&self) -> &EmbeddingProfileId {
        &self.profile_id
    }

    /// Rejects wrong-dimensional, non-finite, zero, and wrongly normalized vectors.
    pub fn validate_vector(&self, vector: &[f32]) -> DiskAnnBackendResult<()> {
        if vector.len() != Self::DIMENSION {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::VectorDimensionMismatch,
            ));
        }
        if vector.iter().any(|value| !value.is_finite()) {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::NonFiniteVector,
            ));
        }
        if vector
            .iter()
            .all(|value| value.to_bits() & 0x7fff_ffff == 0)
        {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::ZeroVector,
            ));
        }

        if self.metric.normalization() == VectorNormalization::UnitL2 {
            let norm = vector
                .iter()
                .map(|value| f64::from(*value).powi(2))
                .sum::<f64>()
                .sqrt();
            if (norm - 1.0).abs() > Self::COSINE_UNIT_LENGTH_TOLERANCE {
                return Err(DiskAnnBackendError::contract(
                    DiskAnnBackendDiagnosticCode::MetricNormalizationMismatch,
                ));
            }
        }
        Ok(())
    }
}
