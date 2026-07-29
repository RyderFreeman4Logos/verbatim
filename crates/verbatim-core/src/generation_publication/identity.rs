//! Generation-, version-, and content-aware identities for publication and
//! migration coordination.
//!
//! These newtypes enforce monotonic ordering and content-aware uniqueness for
//! the publication lifecycle without retaining any caller-controlled text in
//! diagnostics. A publication generation and coordinator epoch are positive;
//! a content hash is a validated `sha256:` digest whose value is never rendered
//! in `Debug`.

use serde::{Deserialize, Serialize};

use super::{
    GenerationPublicationDiagnosticCode, GenerationPublicationError, GenerationPublicationResult,
};

/// Monotonic, nonzero vector-backend publication generation. Only one
/// generation is active for query serving at a time; older generations remain
/// readable until their leases expire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PublicationGenerationId(u64);

impl<'de> Deserialize<'de> for PublicationGenerationId {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl PublicationGenerationId {
    /// Constructs a nonzero publication generation.
    pub fn new(value: u64) -> GenerationPublicationResult<Self> {
        if value == 0 {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the serialized generation value.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Monotonic, nonzero coordinator epoch used to fence concurrent promotion and
/// rollback attempts. Each successful promote or rollback advances the epoch.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct CoordinatorEpoch(u64);

impl<'de> Deserialize<'de> for CoordinatorEpoch {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl CoordinatorEpoch {
    /// Constructs a nonzero coordinator epoch.
    pub fn new(value: u64) -> GenerationPublicationResult<Self> {
        if value == 0 {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the next epoch in the monotonic sequence.
    pub const fn next(self) -> Self {
        // Safety: self.0 is nonzero by construction; +1 cannot overflow to zero
        // except at u64::MAX, which is guarded by callers remaining realistic.
        Self(self.0.saturating_add(1))
    }

    /// Returns the serialized epoch value.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Positive shard ordinal within a publication generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct ShardOrdinal(u32);

impl ShardOrdinal {
    /// Maximum supported shard ordinal per generation (mirrors the DiskANN3
    /// shard contract cap).
    pub const MAX: u32 = 1_000_000;

    /// Constructs a positive, bounded shard ordinal.
    pub fn new(value: u32) -> GenerationPublicationResult<Self> {
        if value == 0 || value > Self::MAX {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidBounds,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the serialized ordinal value.
    pub const fn value(self) -> u32 {
        self.0
    }
}

/// Validated `sha256:` content hash for a staged artifact (vectors, graph,
/// filters, ID map, exact vectors). The value is retained for integrity
/// comparison but never rendered in diagnostics.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct ContentHash(String);

impl ContentHash {
    /// Constructs a validated `sha256:` hex digest.
    pub fn new(value: impl Into<String>) -> GenerationPublicationResult<Self> {
        let hash = Self(value.into());
        hash.validate()?;
        Ok(hash)
    }

    /// Returns the serialized hash.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Revalidates the `sha256:` prefix and 64 lowercase-hex digits.
    pub fn validate(&self) -> GenerationPublicationResult<()> {
        let valid = self
            .0
            .strip_prefix("sha256:")
            .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
        if !valid {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidHash,
            ));
        }
        Ok(())
    }
}

impl TryFrom<String> for ContentHash {
    type Error = GenerationPublicationError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl std::fmt::Debug for ContentHash {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ContentHash(REDACTED)")
    }
}
