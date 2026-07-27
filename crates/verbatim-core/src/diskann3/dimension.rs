//! Full-precision vector dimension validation.

use serde::{Deserialize, Serialize};

use super::{VectorSearchDiagnosticCode, VectorSearchError, VectorSearchResult};

/// The only supported embedding dimension: 4,096 finite `f32` values.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VectorDimension(u32);

impl VectorDimension {
    /// Number of original full-precision float32 dimensions retained per vector.
    pub const FULL_PRECISION_F32: usize = 4_096;
    /// Typed representation of [`Self::FULL_PRECISION_F32`].
    pub const FULL_PRECISION: Self = Self(4_096);

    pub fn new(value: usize) -> VectorSearchResult<Self> {
        if value != Self::FULL_PRECISION_F32 {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::DimensionMismatch,
            ));
        }
        Ok(Self::FULL_PRECISION)
    }

    pub const fn value(self) -> u32 {
        self.0
    }

    /// Rejects short, overlong, NaN, and infinite query vectors.
    pub fn validate_vector(vector: &[f32]) -> VectorSearchResult<()> {
        if vector.len() != Self::FULL_PRECISION_F32 || vector.iter().any(|value| !value.is_finite())
        {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::DimensionMismatch,
            ));
        }
        Ok(())
    }
}
