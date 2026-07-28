//! Compact stable numeric shard identifiers and versioned generation keys.
//!
//! Shard identity is composed of operational dimensions (vector space, encoder
//! profile, index schema version, publication generation, and shard ordinal),
//! never by individual source. A shard contains many sources and tenants subject
//! to authorization policy.

use serde::{Deserialize, Serialize};

use super::{VectorShardDiagnosticCode, VectorShardError, VectorShardResult};

/// Monotonic nonzero publication generation of an immutable shard set.
///
/// Generations gate atomic read and rollback: a reader binds to one generation
/// and cannot combine shards across generations. Zero is prohibited and the
/// prohibition is enforced through the constructor, even when deserialized.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ShardGeneration(u64);

impl<'de> Deserialize<'de> for ShardGeneration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl ShardGeneration {
    /// Constructs a nonzero generation; the only route to a valid value.
    pub fn new(value: u64) -> VectorShardResult<Self> {
        if value == 0 {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidGeneration,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the serialized numeric generation.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Named vector space. Sources are members, not independent indexes.
///
/// One-index-per-source is prohibited because it causes file/handle proliferation
/// and unbounded query fan-out as source count grows.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct ShardVectorSpace(String);

impl ShardVectorSpace {
    /// Constructs a validated named vector space.
    pub fn new(value: impl Into<String>) -> VectorShardResult<Self> {
        let id = Self(value.into());
        id.validate()?;
        Ok(id)
    }

    /// Returns the vector-space name.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Revalidates the name's charset and length bounds.
    pub fn validate(&self) -> VectorShardResult<()> {
        let allowed = self.0.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        });
        if self.0.is_empty() || self.0.len() > 128 || !allowed {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidShardId,
            ));
        }
        Ok(())
    }
}

impl TryFrom<String> for ShardVectorSpace {
    type Error = VectorShardError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

/// Compact, nonzero shard ordinal within one generation and vector space.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct ShardOrdinal(u32);

impl<'de> Deserialize<'de> for ShardOrdinal {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u32::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl ShardOrdinal {
    /// Hard maximum on the number of shards within a single generation.
    pub const MAX_ORDINAL: u32 = 1_000_000;

    /// Constructs a bounded nonzero shard ordinal.
    pub fn new(value: u32) -> VectorShardResult<Self> {
        if value == 0 || value > Self::MAX_ORDINAL {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidShardId,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the serialized ordinal.
    pub const fn value(self) -> u32 {
        self.0
    }
}

/// Immutable shard key scoped to a vector space and one publication generation.
///
/// Combines the operational identity dimensions that allow SSD usage to grow
/// linearly while open-file state and online memory stay bounded.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ShardId {
    vector_space: ShardVectorSpace,
    generation: ShardGeneration,
    ordinal: ShardOrdinal,
}

impl ShardId {
    /// Constructs a fully validated shard key.
    pub fn new(
        vector_space: ShardVectorSpace,
        generation: ShardGeneration,
        ordinal: ShardOrdinal,
    ) -> VectorShardResult<Self> {
        let shard = Self {
            vector_space,
            generation,
            ordinal,
        };
        shard.validate()?;
        Ok(shard)
    }

    /// Revalidates the shard's identity envelope.
    pub fn validate(&self) -> VectorShardResult<()> {
        self.vector_space.validate()?;
        // Generation and ordinal are already nonzero-by-construction newtypes,
        // but re-check defensively in case of future internal mutation.
        if self.generation.value() == 0 || self.ordinal.value() == 0 {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidShardId,
            ));
        }
        Ok(())
    }

    /// Returns the vector space this shard belongs to.
    pub const fn vector_space(&self) -> &ShardVectorSpace {
        &self.vector_space
    }

    /// Returns the publication generation of this shard.
    pub const fn generation(&self) -> ShardGeneration {
        self.generation
    }

    /// Returns the shard ordinal within its generation.
    pub const fn ordinal(&self) -> ShardOrdinal {
        self.ordinal
    }
}
