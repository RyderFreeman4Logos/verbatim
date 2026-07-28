//! Immutable vector-space, generation, shard, and SSD-manifest contracts.

use serde::{Deserialize, Serialize};

use super::{VectorDimension, VectorSearchDiagnosticCode, VectorSearchError, VectorSearchResult};

/// Named vector space. Sources are members, not independent indexes.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct VectorSpaceId(String);

impl VectorSpaceId {
    pub fn new(value: impl Into<String>) -> VectorSearchResult<Self> {
        let id = Self(value.into());
        id.validate()?;
        Ok(id)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    pub fn validate(&self) -> VectorSearchResult<()> {
        let allowed = self.0.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'-' | b'_')
        });
        if self.0.is_empty() || self.0.len() > 128 || !allowed {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::ShardCorrupt,
            ));
        }
        Ok(())
    }
}

/// Monotonic, nonzero index publication generation used for atomic reads and rollback.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PublicationGeneration(u64);

impl<'de> Deserialize<'de> for PublicationGeneration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl PublicationGeneration {
    pub fn new(value: u64) -> VectorSearchResult<Self> {
        if value == 0 {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::GenerationMismatch,
            ));
        }
        Ok(Self(value))
    }

    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Immutable shard key scoped to a vector-space and one publication generation.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ShardId {
    pub vector_space: VectorSpaceId,
    pub generation: PublicationGeneration,
    pub ordinal: u32,
}

impl ShardId {
    pub const MAX_ORDINAL: u32 = 1_000_000;

    pub fn new(
        vector_space: VectorSpaceId,
        generation: PublicationGeneration,
        ordinal: u32,
    ) -> VectorSearchResult<Self> {
        let shard = Self {
            vector_space,
            generation,
            ordinal,
        };
        shard.validate()?;
        Ok(shard)
    }

    pub fn validate(&self) -> VectorSearchResult<()> {
        self.vector_space.validate()?;
        if self.ordinal > Self::MAX_ORDINAL {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::ShardCorrupt,
            ));
        }
        Ok(())
    }
}

/// Candidate-vector compression representation. Original float32 vectors stay authoritative.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QuantizerType {
    None,
    ScalarQuantizedCandidate,
    ProductQuantizedCandidate,
}

/// SSD page layout selected by a future DataProvider implementation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SsdPageLayout {
    AisaqCoLocated,
    SeparateGraphAndVectors,
}

/// Bounded integrity identifier for a published SSD shard.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ShardChecksum(String);

impl ShardChecksum {
    pub fn new(value: impl Into<String>) -> VectorSearchResult<Self> {
        let checksum = Self(value.into());
        if checksum.0.is_empty() || checksum.0.len() > 256 || !checksum.0.is_ascii() {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::ShardCorrupt,
            ));
        }
        Ok(checksum)
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }

    fn is_sha256(&self) -> bool {
        self.0.strip_prefix("sha256:").is_some_and(|value| {
            value.len() == 64 && value.bytes().all(|byte| byte.is_ascii_hexdigit())
        })
    }
}

/// Serializable metadata for one immutable, bounded-size SSD shard.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SsdShardManifest {
    pub shard: ShardId,
    pub vector_count: u64,
    pub dimension: VectorDimension,
    pub byte_size: u64,
    pub graph_degree: u16,
    pub quantizer: QuantizerType,
    pub page_layout: SsdPageLayout,
    pub checksum: ShardChecksum,
}

/// Construction fields for [`SsdShardManifest`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SsdShardManifestFields {
    pub shard: ShardId,
    pub vector_count: u64,
    pub dimension: VectorDimension,
    pub byte_size: u64,
    pub graph_degree: u16,
    pub quantizer: QuantizerType,
    pub page_layout: SsdPageLayout,
    pub checksum: ShardChecksum,
}

impl SsdShardManifest {
    pub const MAX_VECTORS_PER_SHARD: u64 = 10_000_000;

    pub fn new(fields: SsdShardManifestFields) -> VectorSearchResult<Self> {
        let manifest = Self {
            shard: fields.shard,
            vector_count: fields.vector_count,
            dimension: fields.dimension,
            byte_size: fields.byte_size,
            graph_degree: fields.graph_degree,
            quantizer: fields.quantizer,
            page_layout: fields.page_layout,
            checksum: fields.checksum,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> VectorSearchResult<()> {
        self.shard.validate()?;
        if self.vector_count == 0
            || self.vector_count > Self::MAX_VECTORS_PER_SHARD
            || self.dimension != VectorDimension::FULL_PRECISION
            || self.graph_degree == 0
            || self.graph_degree > 128
            || !self.checksum.is_sha256()
        {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::ShardCorrupt,
            ));
        }
        let minimum_float_bytes = self
            .vector_count
            .checked_mul(u64::from(self.dimension.value()))
            .and_then(|bytes| bytes.checked_mul(4));
        if minimum_float_bytes.is_none_or(|minimum| self.byte_size < minimum) {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::ShardCorrupt,
            ));
        }
        Ok(())
    }
}

pub fn encode_ssd_shard_manifest_json(manifest: &SsdShardManifest) -> VectorSearchResult<String> {
    manifest.validate()?;
    serde_json::to_string(manifest)
        .map_err(|_| VectorSearchError::contract(VectorSearchDiagnosticCode::SerializationFailed))
}

pub fn decode_ssd_shard_manifest_json(input: &str) -> VectorSearchResult<SsdShardManifest> {
    let manifest: SsdShardManifest = serde_json::from_str(input)
        .map_err(|_| VectorSearchError::contract(VectorSearchDiagnosticCode::InvalidManifest))?;
    manifest
        .validate()
        .map_err(|_| VectorSearchError::contract(VectorSearchDiagnosticCode::InvalidManifest))?;
    Ok(manifest)
}
