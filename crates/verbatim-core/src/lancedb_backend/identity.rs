//! Bounded table, profile, digest, generation, and hydration identities.

use serde::{Deserialize, Serialize};

use crate::diskann3::PublicationGeneration;
use crate::types::EmbeddingProfileId;

use super::{LanceDbBackendDiagnosticCode, LanceDbBackendError, LanceDbBackendResult};

const MAX_TABLE_NAME_LEN: usize = 128;
const CONFIG_DIGEST_LEN: usize = 64;
const MAX_CHUNK_ID_LEN: usize = 128;

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize)]
#[serde(transparent)]
pub struct TableName(String);

impl<'de> Deserialize<'de> for TableName {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        Self::new(String::deserialize(deserializer)?).map_err(serde::de::Error::custom)
    }
}

impl TableName {
    pub fn new(value: impl Into<String>) -> LanceDbBackendResult<Self> {
        let value = value.into();
        if !is_bounded_id(&value, MAX_TABLE_NAME_LEN) {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::InvalidTableName,
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Identity binding for every table/index operation and publication generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanceDbCollectionIdentity {
    table: TableName,
    profile_id: EmbeddingProfileId,
    generation: PublicationGeneration,
    config_digest: String,
}

impl<'de> Deserialize<'de> for LanceDbCollectionIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            table: TableName,
            profile_id: String,
            generation: PublicationGeneration,
            config_digest: String,
        }
        let wire = Wire::deserialize(deserializer)?;
        let profile_id = EmbeddingProfileId::new(wire.profile_id).map_err(|_| {
            serde::de::Error::custom(
                LanceDbBackendError::contract(LanceDbBackendDiagnosticCode::InvalidProfileId)
                    .to_string(),
            )
        })?;
        Self::new(wire.table, profile_id, wire.generation, wire.config_digest)
            .map_err(serde::de::Error::custom)
    }
}

impl LanceDbCollectionIdentity {
    pub fn new(
        table: TableName,
        profile_id: EmbeddingProfileId,
        generation: PublicationGeneration,
        config_digest: impl Into<String>,
    ) -> LanceDbBackendResult<Self> {
        let config_digest = config_digest.into();
        if generation.value() == 0 {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::InvalidGeneration,
            ));
        }
        if profile_id.as_str().is_empty() {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::InvalidProfileId,
            ));
        }
        if config_digest.len() != CONFIG_DIGEST_LEN
            || !config_digest
                .bytes()
                .all(|byte| byte.is_ascii_hexdigit() && !byte.is_ascii_uppercase())
        {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::InvalidConfigDigest,
            ));
        }
        Ok(Self {
            table,
            profile_id,
            generation,
            config_digest,
        })
    }

    pub const fn table(&self) -> &TableName {
        &self.table
    }

    pub fn profile_id(&self) -> &EmbeddingProfileId {
        &self.profile_id
    }

    pub const fn generation(&self) -> PublicationGeneration {
        self.generation
    }

    pub fn config_digest(&self) -> &str {
        &self.config_digest
    }

    pub fn validate_hydration(&self, hit: &LanceDbHitRef) -> LanceDbBackendResult<()> {
        if hit.generation.value() < self.generation.value() {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::StaleGenerationHydration,
            ));
        }
        if hit.generation != self.generation || hit.profile_id != self.profile_id.as_str() {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::WrongGenerationHydration,
            ));
        }
        Ok(())
    }
}

/// Generation-tagged candidate which cannot hydrate against another published table generation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanceDbHitRef {
    chunk_id: String,
    profile_id: String,
    generation: PublicationGeneration,
}

impl<'de> Deserialize<'de> for LanceDbHitRef {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            chunk_id: String,
            profile_id: String,
            generation: PublicationGeneration,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.chunk_id, wire.profile_id, wire.generation).map_err(serde::de::Error::custom)
    }
}

impl LanceDbHitRef {
    pub fn new(
        chunk_id: impl Into<String>,
        profile_id: impl Into<String>,
        generation: PublicationGeneration,
    ) -> LanceDbBackendResult<Self> {
        let chunk_id = chunk_id.into();
        let profile_id = profile_id.into();
        if chunk_id.is_empty()
            || chunk_id.len() > MAX_CHUNK_ID_LEN
            || profile_id.is_empty()
            || generation.value() == 0
        {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::WrongGenerationHydration,
            ));
        }
        Ok(Self {
            chunk_id,
            profile_id,
            generation,
        })
    }

    pub fn chunk_id(&self) -> &str {
        &self.chunk_id
    }
}

fn is_bounded_id(value: &str, max_len: usize) -> bool {
    !value.is_empty()
        && value.len() <= max_len
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'-')
        })
}
