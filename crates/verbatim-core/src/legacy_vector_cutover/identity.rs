//! Durable, constructor-validated cutover identities and manifest.

use serde::{Deserialize, Serialize};

use super::{LegacyRetirementDiagnosticCode, LegacyRetirementError, LegacyRetirementResult};

/// Schema version for a durable legacy-vector-cutover manifest.
pub const LEGACY_VECTOR_CUTOVER_SCHEMA_VERSION: u32 = 1;

/// A validated, opaque publication-generation identity.
#[derive(Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
#[serde(transparent)]
pub struct PublicationGeneration(String);

impl<'de> Deserialize<'de> for PublicationGeneration {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

impl PublicationGeneration {
    /// Constructs a bounded, printable generation identity.
    pub fn new(value: impl Into<String>) -> LegacyRetirementResult<Self> {
        let value = value.into();
        let valid = !value.is_empty()
            && value.len() <= 128
            && value.bytes().all(|byte| byte.is_ascii_graphic());
        if !valid {
            return Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the persisted generation identity.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl std::fmt::Debug for PublicationGeneration {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("PublicationGeneration(REDACTED)")
    }
}

/// Constructor input for a durable cutover manifest.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CutoverManifestFields {
    /// Must match [`LEGACY_VECTOR_CUTOVER_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// Serving generation retained through the rollback window.
    pub incumbent_generation: String,
    /// Candidate DiskANN3 generation that was shadowed and promoted.
    pub candidate_generation: String,
    /// Inclusive logical time when the incumbent is eligible for retirement.
    pub rollback_window_end: u64,
}

/// A durable record binding an incumbent to one promoted DiskANN3 generation.
#[derive(Clone, PartialEq, Eq, Serialize)]
pub struct CutoverManifest {
    schema_version: u32,
    incumbent_generation: PublicationGeneration,
    candidate_generation: PublicationGeneration,
    rollback_window_end: u64,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CutoverManifestWire {
    schema_version: u32,
    incumbent_generation: String,
    candidate_generation: String,
    rollback_window_end: u64,
}

impl<'de> Deserialize<'de> for CutoverManifest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = CutoverManifestWire::deserialize(deserializer)?;
        Self::new(CutoverManifestFields {
            schema_version: wire.schema_version,
            incumbent_generation: wire.incumbent_generation,
            candidate_generation: wire.candidate_generation,
            rollback_window_end: wire.rollback_window_end,
        })
        .map_err(serde::de::Error::custom)
    }
}

impl CutoverManifest {
    /// Validates and constructs the durable promotion/rollback binding.
    pub fn new(fields: CutoverManifestFields) -> LegacyRetirementResult<Self> {
        if fields.schema_version != LEGACY_VECTOR_CUTOVER_SCHEMA_VERSION
            || fields.rollback_window_end == 0
        {
            return Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::InvalidManifest,
            ));
        }
        let incumbent_generation = PublicationGeneration::new(fields.incumbent_generation)?;
        let candidate_generation = PublicationGeneration::new(fields.candidate_generation)?;
        if incumbent_generation == candidate_generation {
            return Err(LegacyRetirementError::contract(
                LegacyRetirementDiagnosticCode::InvalidManifest,
            ));
        }
        Ok(Self {
            schema_version: fields.schema_version,
            incumbent_generation,
            candidate_generation,
            rollback_window_end: fields.rollback_window_end,
        })
    }

    /// Returns the durable schema version.
    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    /// Returns the retained incumbent generation.
    pub const fn incumbent_generation(&self) -> &PublicationGeneration {
        &self.incumbent_generation
    }

    /// Returns the promoted candidate generation.
    pub const fn candidate_generation(&self) -> &PublicationGeneration {
        &self.candidate_generation
    }

    /// Returns the inclusive retirement-eligibility logical time.
    pub const fn rollback_window_end(&self) -> u64 {
        self.rollback_window_end
    }
}

impl std::fmt::Debug for CutoverManifest {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("CutoverManifest(REDACTED)")
    }
}
