//! Generation-, version-, and content-aware identities for durable mutations.
//!
//! These newtypes enforce monotonic ordering and content-aware uniqueness for
//! the update lifecycle without retaining any caller-controlled text. A stable
//! vector id is a positive number; a generation and mapping version are nonzero;
//! a content hash is a validated `sha256:` digest whose value is never rendered
//! in diagnostics.

use serde::{Deserialize, Serialize};

use super::{DurableUpdateDiagnosticCode, DurableUpdateError, DurableUpdateResult};

/// Monotonic, nonzero durable update generation. Old generations remain readable
/// until their leases expire; a new generation is the only one eligible for
/// publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct DurableGeneration(u64);

impl<'de> Deserialize<'de> for DurableGeneration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u64::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl DurableGeneration {
    /// Constructs a nonzero generation.
    pub fn new(value: u64) -> DurableUpdateResult<Self> {
        if value == 0 {
            return Err(DurableUpdateError::contract(
                DurableUpdateDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the serialized generation value.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Positive, monotonically increasing mapping/operation version. Two operations
/// that touch the same vector id must carry strictly increasing versions; a
/// stale version is rejected before it reaches the provider.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct MutationVersion(u32);

impl<'de> Deserialize<'de> for MutationVersion {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = u32::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl MutationVersion {
    /// Constructs a nonzero version.
    pub fn new(value: u32) -> DurableUpdateResult<Self> {
        if value == 0 {
            return Err(DurableUpdateError::contract(
                DurableUpdateDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the serialized version value.
    pub const fn value(self) -> u32 {
        self.0
    }
}

/// Stable positive numeric identity of a vector within a durable generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(transparent)]
pub struct DurableVectorId(u64);

impl DurableVectorId {
    /// Constructs a nonzero stable vector identity.
    pub fn new(value: u64) -> DurableUpdateResult<Self> {
        if value == 0 {
            return Err(DurableUpdateError::contract(
                DurableUpdateDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the serialized numeric identity.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Validated `sha256:` content hash for a vector or source snapshot. The value
/// is retained for integrity comparison but never rendered in diagnostics.
#[derive(Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(try_from = "String")]
pub struct ContentHash(String);

impl ContentHash {
    /// Constructs a validated `sha256:` hex digest.
    pub fn new(value: impl Into<String>) -> DurableUpdateResult<Self> {
        let hash = Self(value.into());
        hash.validate()?;
        Ok(hash)
    }

    /// Returns the serialized hash.
    pub fn as_str(&self) -> &str {
        &self.0
    }

    /// Revalidates the `sha256:` prefix and 64 lowercase-hex digits.
    pub fn validate(&self) -> DurableUpdateResult<()> {
        let valid = self
            .0
            .strip_prefix("sha256:")
            .is_some_and(|hex| hex.len() == 64 && hex.bytes().all(|b| b.is_ascii_hexdigit()));
        if !valid {
            return Err(DurableUpdateError::contract(
                DurableUpdateDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(())
    }
}

impl TryFrom<String> for ContentHash {
    type Error = DurableUpdateError;

    fn try_from(value: String) -> Result<Self, Self::Error> {
        Self::new(value)
    }
}

impl std::fmt::Debug for ContentHash {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("ContentHash(REDACTED)")
    }
}
