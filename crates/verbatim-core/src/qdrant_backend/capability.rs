//! Health and capability discovery for the Qdrant reference backend.

use serde::{Deserialize, Serialize};

use super::{QdrantBackendDiagnosticCode, QdrantBackendError, QdrantBackendResult};

/// Explicit capability envelope advertised by one Qdrant adapter instance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QdrantCapabilityFields {
    pub supports_query_api: bool,
    pub supports_named_vectors: bool,
    pub supports_multivector: bool,
    pub supports_quantization: bool,
    pub supports_on_disk_vectors: bool,
    pub supports_on_disk_hnsw: bool,
    pub supports_sparse_vectors: bool,
    /// Control-plane flag only: does not claim Tantivy replacement.
    pub sparse_bm25_control_enabled: bool,
    pub supports_payload_indexes: bool,
    pub supports_grpc: bool,
    pub max_retries: u16,
    pub health_deadline_micros: u64,
}

/// Validated immutable Qdrant capability envelope.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QdrantCapabilities {
    fields: QdrantCapabilityFields,
}

impl<'de> Deserialize<'de> for QdrantCapabilities {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = QdrantCapabilityFields::deserialize(deserializer)?;
        Self::new(fields).map_err(serde::de::Error::custom)
    }
}

impl QdrantCapabilities {
    /// Rejects incomplete enterprise reference capability claims.
    pub fn new(fields: QdrantCapabilityFields) -> QdrantBackendResult<Self> {
        if !fields.supports_query_api
            || !fields.supports_named_vectors
            || !fields.supports_payload_indexes
            || !fields.supports_grpc
            || fields.max_retries == 0
            || fields.health_deadline_micros == 0
        {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidCapabilities,
            ));
        }
        Ok(Self { fields })
    }

    pub const fn fields(&self) -> &QdrantCapabilityFields {
        &self.fields
    }
}
