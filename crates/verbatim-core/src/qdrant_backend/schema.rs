//! Collection schema validation contract for the Qdrant reference backend.

use serde::{Deserialize, Serialize};

use super::{
    QdrantBackendDiagnosticCode, QdrantBackendError, QdrantBackendResult, QdrantCollectionIdentity,
};

/// Similarity metric enforced for a named vector space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QdrantVectorMetric {
    Cosine,
    Dot,
    Euclid,
}

/// Normalization behavior required before vectors may enter the collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QdrantVectorNormalization {
    UnitL2,
    PreserveMagnitude,
}

impl QdrantVectorMetric {
    pub const fn normalization(self) -> QdrantVectorNormalization {
        match self {
            Self::Cosine => QdrantVectorNormalization::UnitL2,
            Self::Dot | Self::Euclid => QdrantVectorNormalization::PreserveMagnitude,
        }
    }
}

/// Quantization profile advertised by the collection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuantizationProfile {
    None,
    Scalar,
    Product,
    Binary,
}

/// Fields describing one validated Qdrant collection schema.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QdrantSchemaFields {
    pub dimension: u32,
    pub metric: QdrantVectorMetric,
    pub normalization: QdrantVectorNormalization,
    pub quantization: QuantizationProfile,
    pub hnsw_on_disk: bool,
    pub vectors_on_disk: bool,
    pub payload_on_disk: bool,
    pub requires_payload_schema: bool,
}

/// Validated schema bound to a collection identity.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QdrantCollectionSchema {
    identity: QdrantCollectionIdentity,
    fields: QdrantSchemaFields,
}

impl<'de> Deserialize<'de> for QdrantCollectionSchema {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            identity: QdrantCollectionIdentity,
            fields: QdrantSchemaFields,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.identity, wire.fields).map_err(serde::de::Error::custom)
    }
}

impl QdrantCollectionSchema {
    /// The only accepted original-vector dimension for this enterprise program.
    pub const DIMENSION: u32 = 4_096;

    /// Validates dimension, metric/normalization pairing, and identity binding.
    pub fn new(
        identity: QdrantCollectionIdentity,
        fields: QdrantSchemaFields,
    ) -> QdrantBackendResult<Self> {
        if fields.dimension != Self::DIMENSION {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::VectorDimensionMismatch,
            ));
        }
        if fields.metric.normalization() != fields.normalization {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidSchema,
            ));
        }
        if !fields.requires_payload_schema {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidSchema,
            ));
        }
        Ok(Self { identity, fields })
    }

    pub const fn identity(&self) -> &QdrantCollectionIdentity {
        &self.identity
    }

    pub const fn fields(&self) -> &QdrantSchemaFields {
        &self.fields
    }

    pub const fn dimension(&self) -> u32 {
        self.fields.dimension
    }
}
