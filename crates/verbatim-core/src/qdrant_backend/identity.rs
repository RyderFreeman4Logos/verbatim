//! Bounded, validated collection / named-vector / profile / generation identity.

use serde::{Deserialize, Serialize};

use crate::diskann3::PublicationGeneration;
use crate::types::EmbeddingProfileId;

use super::{QdrantBackendDiagnosticCode, QdrantBackendError, QdrantBackendResult};

const MAX_COLLECTION_NAME_LEN: usize = 128;
const MAX_NAMED_VECTOR_SPACE_LEN: usize = 128;
const CONFIG_DIGEST_LEN: usize = 64;

/// Qdrant collection name used by one enterprise reference deployment.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct CollectionName(String);

impl<'de> Deserialize<'de> for CollectionName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl CollectionName {
    pub fn new(value: impl Into<String>) -> QdrantBackendResult<Self> {
        let value = value.into();
        if !is_bounded_id(&value, MAX_COLLECTION_NAME_LEN) {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidCollectionName,
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Named vector space identity (Qdrant named vector key).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct NamedVectorSpaceId(String);

impl<'de> Deserialize<'de> for NamedVectorSpaceId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl NamedVectorSpaceId {
    pub fn new(value: impl Into<String>) -> QdrantBackendResult<Self> {
        let value = value.into();
        if !is_bounded_id(&value, MAX_NAMED_VECTOR_SPACE_LEN) {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidNamedVectorSpace,
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Hex-encoded configuration digest bound into schema validation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct ConfigDigest(String);

impl<'de> Deserialize<'de> for ConfigDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl ConfigDigest {
    pub fn new(value: impl Into<String>) -> QdrantBackendResult<Self> {
        let value = value.into();
        if value.len() != CONFIG_DIGEST_LEN
            || !value
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidConfigDigest,
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Full identity envelope for one Qdrant reference collection generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QdrantCollectionIdentity {
    collection: CollectionName,
    named_vector: NamedVectorSpaceId,
    profile_id: EmbeddingProfileId,
    generation: PublicationGeneration,
    config_digest: ConfigDigest,
}

impl<'de> Deserialize<'de> for QdrantCollectionIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            collection: CollectionName,
            named_vector: NamedVectorSpaceId,
            profile_id: String,
            generation: PublicationGeneration,
            config_digest: ConfigDigest,
        }
        let wire = Wire::deserialize(deserializer)?;
        let profile_id = EmbeddingProfileId::new(wire.profile_id).map_err(|_| {
            serde::de::Error::custom(
                QdrantBackendError::contract(QdrantBackendDiagnosticCode::InvalidProfileId)
                    .to_string(),
            )
        })?;
        Self::new(
            wire.collection,
            wire.named_vector,
            profile_id,
            wire.generation,
            wire.config_digest,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl QdrantCollectionIdentity {
    pub fn new(
        collection: CollectionName,
        named_vector: NamedVectorSpaceId,
        profile_id: EmbeddingProfileId,
        generation: PublicationGeneration,
        config_digest: ConfigDigest,
    ) -> QdrantBackendResult<Self> {
        if generation.value() == 0 {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidGeneration,
            ));
        }
        if profile_id.as_str().is_empty() {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidProfileId,
            ));
        }
        Ok(Self {
            collection,
            named_vector,
            profile_id,
            generation,
            config_digest,
        })
    }

    pub const fn collection(&self) -> &CollectionName {
        &self.collection
    }

    pub const fn named_vector(&self) -> &NamedVectorSpaceId {
        &self.named_vector
    }

    pub fn profile_id(&self) -> &EmbeddingProfileId {
        &self.profile_id
    }

    pub const fn generation(&self) -> PublicationGeneration {
        self.generation
    }

    pub const fn config_digest(&self) -> &ConfigDigest {
        &self.config_digest
    }
}

fn is_bounded_id(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        })
}
