//! Types-only description of the official client / gRPC / Query API path.

use serde::{Deserialize, Serialize};

use super::{QdrantBackendDiagnosticCode, QdrantBackendError, QdrantBackendResult};

/// Transport required for the enterprise performance path.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QdrantTransport {
    /// Official Rust client over gRPC (required for the performance path).
    OfficialClientGrpc,
    /// Transitional hand-written REST remains outside this contract module.
    TransitionalRestLegacy,
}

/// Query surface required for named-vector enterprise search.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QdrantQuerySurface {
    QueryApi,
    LegacyPointsSearch,
}

/// Types-only requirements for the future gRPC cutover path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GrpcPathRequirements {
    transport: QdrantTransport,
    query_surface: QdrantQuerySurface,
    requires_named_vectors: bool,
    requires_payload_indexes: bool,
}

impl<'de> Deserialize<'de> for GrpcPathRequirements {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            transport: QdrantTransport,
            query_surface: QdrantQuerySurface,
            requires_named_vectors: bool,
            requires_payload_indexes: bool,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.transport,
            wire.query_surface,
            wire.requires_named_vectors,
            wire.requires_payload_indexes,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl GrpcPathRequirements {
    /// Accepts only the official gRPC + Query API path as the enterprise target.
    pub fn new(
        transport: QdrantTransport,
        query_surface: QdrantQuerySurface,
        requires_named_vectors: bool,
        requires_payload_indexes: bool,
    ) -> QdrantBackendResult<Self> {
        if transport != QdrantTransport::OfficialClientGrpc
            || query_surface != QdrantQuerySurface::QueryApi
            || !requires_named_vectors
            || !requires_payload_indexes
        {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidGrpcPathRequirements,
            ));
        }
        Ok(Self {
            transport,
            query_surface,
            requires_named_vectors,
            requires_payload_indexes,
        })
    }

    pub const fn transport(&self) -> QdrantTransport {
        self.transport
    }

    pub const fn query_surface(&self) -> QdrantQuerySurface {
        self.query_surface
    }

    pub const fn requires_named_vectors(&self) -> bool {
        self.requires_named_vectors
    }

    pub const fn requires_payload_indexes(&self) -> bool {
        self.requires_payload_indexes
    }
}
