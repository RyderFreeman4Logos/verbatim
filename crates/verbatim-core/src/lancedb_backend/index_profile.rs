//! Closed LanceDB vector-index profiles for comparison under VECTOR-REF-002.

use serde::{Deserialize, Serialize};

use super::{LanceDbBackendDiagnosticCode, LanceDbBackendError, LanceDbBackendResult};

/// Candidate-generation profile. HNSW variants are controls, never implicit defaults.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LanceDbIndexProfile {
    IvfRq,
    IvfPq { num_sub_vectors: u16 },
    IvfHnswFlat,
    IvfHnswSq,
    BypassExactScan,
}

impl<'de> Deserialize<'de> for LanceDbIndexProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        #[serde(rename_all = "snake_case")]
        enum Wire {
            IvfRq,
            IvfPq { num_sub_vectors: u16 },
            IvfHnswFlat,
            IvfHnswSq,
            BypassExactScan,
        }
        match Wire::deserialize(deserializer)? {
            Wire::IvfRq => Ok(Self::IvfRq),
            Wire::IvfPq { num_sub_vectors } => {
                Self::ivf_pq(num_sub_vectors).map_err(serde::de::Error::custom)
            }
            Wire::IvfHnswFlat => Ok(Self::IvfHnswFlat),
            Wire::IvfHnswSq => Ok(Self::IvfHnswSq),
            Wire::BypassExactScan => Ok(Self::BypassExactScan),
        }
    }
}

impl LanceDbIndexProfile {
    pub const MAX_PQ_SUB_VECTORS: u16 = 256;

    /// Creates the second quantized baseline only when its partitioning is bounded.
    pub fn ivf_pq(num_sub_vectors: u16) -> LanceDbBackendResult<Self> {
        if num_sub_vectors == 0
            || num_sub_vectors > Self::MAX_PQ_SUB_VECTORS
            || 4_096 % u32::from(num_sub_vectors) != 0
        {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::InvalidIndexProfile,
            ));
        }
        Ok(Self::IvfPq { num_sub_vectors })
    }

    /// Revalidates the profile when it crosses a durable request boundary.
    pub fn validate(self) -> LanceDbBackendResult<()> {
        match self {
            Self::IvfPq { num_sub_vectors } => Self::ivf_pq(num_sub_vectors).map(|_| ()),
            Self::IvfRq | Self::IvfHnswFlat | Self::IvfHnswSq | Self::BypassExactScan => Ok(()),
        }
    }

    pub const fn is_quantized_candidate_generation(self) -> bool {
        matches!(self, Self::IvfRq | Self::IvfPq { .. })
    }

    pub const fn is_high_recall_control(self) -> bool {
        matches!(self, Self::IvfHnswFlat | Self::IvfHnswSq)
    }

    pub const fn is_exact_scan(self) -> bool {
        matches!(self, Self::BypassExactScan)
    }
}
