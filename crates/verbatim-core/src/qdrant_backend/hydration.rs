//! Authoritative-store hydration gates for Qdrant hits.

use serde::{Deserialize, Serialize};

use crate::diskann3::PublicationGeneration;

use super::{
    QdrantBackendDiagnosticCode, QdrantBackendError, QdrantBackendResult, QdrantCollectionIdentity,
};

const MAX_POINT_ID_LEN: usize = 128;

/// Point returned by Qdrant before authoritative hydration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QdrantPointRef {
    point_id: String,
    generation: PublicationGeneration,
    profile_id: String,
}

impl<'de> Deserialize<'de> for QdrantPointRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            point_id: String,
            generation: PublicationGeneration,
            profile_id: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.point_id, wire.generation, wire.profile_id).map_err(serde::de::Error::custom)
    }
}

impl QdrantPointRef {
    pub fn new(
        point_id: impl Into<String>,
        generation: PublicationGeneration,
        profile_id: impl Into<String>,
    ) -> QdrantBackendResult<Self> {
        let point_id = point_id.into();
        let profile_id = profile_id.into();
        if point_id.is_empty()
            || point_id.len() > MAX_POINT_ID_LEN
            || profile_id.is_empty()
            || generation.value() == 0
        {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidHydrationRequest,
            ));
        }
        Ok(Self {
            point_id,
            generation,
            profile_id,
        })
    }

    pub fn point_id(&self) -> &str {
        &self.point_id
    }

    pub const fn generation(&self) -> PublicationGeneration {
        self.generation
    }

    pub fn profile_id(&self) -> &str {
        &self.profile_id
    }
}

/// Validated hydration request that rejects stale or wrong-generation points.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HydrationRequest {
    identity: QdrantCollectionIdentity,
    points: Vec<QdrantPointRef>,
}

impl<'de> Deserialize<'de> for HydrationRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            identity: QdrantCollectionIdentity,
            points: Vec<QdrantPointRef>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.identity, wire.points).map_err(serde::de::Error::custom)
    }
}

impl HydrationRequest {
    pub const MAX_POINTS: usize = 256;

    pub fn new(
        identity: QdrantCollectionIdentity,
        points: Vec<QdrantPointRef>,
    ) -> QdrantBackendResult<Self> {
        if points.is_empty() || points.len() > Self::MAX_POINTS {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidHydrationRequest,
            ));
        }
        for point in &points {
            Self::validate_point(&identity, point)?;
        }
        Ok(Self { identity, points })
    }

    fn validate_point(
        identity: &QdrantCollectionIdentity,
        point: &QdrantPointRef,
    ) -> QdrantBackendResult<()> {
        if point.profile_id() != identity.profile_id().as_str() {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::WrongGenerationHydration,
            ));
        }
        if point.generation().value() < identity.generation().value() {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::StaleGenerationHydration,
            ));
        }
        if point.generation() != identity.generation() {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::WrongGenerationHydration,
            ));
        }
        Ok(())
    }

    pub const fn identity(&self) -> &QdrantCollectionIdentity {
        &self.identity
    }

    pub fn points(&self) -> &[QdrantPointRef] {
        &self.points
    }
}
