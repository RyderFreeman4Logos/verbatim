//! Types-only publication, tombstone, and independent-retention lifecycle.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{
    NamedVectorSpaceDiagnosticCode, NamedVectorSpaceError, NamedVectorSpaceId,
    NamedVectorSpaceResult, PublicationGeneration,
};

/// Explicit manifest state for every planned named space at atomic publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpacePublicationState {
    Complete,
    Optional,
    Unavailable,
    Stale,
}

/// One staged per-space artifact and its compact object mapping artifact.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct StagedSpaceArtifact {
    space: NamedVectorSpaceId,
    generation: PublicationGeneration,
    mapping_artifact_count: u64,
    state: SpacePublicationState,
}

impl<'de> Deserialize<'de> for StagedSpaceArtifact {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            space: NamedVectorSpaceId,
            generation: PublicationGeneration,
            mapping_artifact_count: u64,
            state: SpacePublicationState,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.space,
            wire.generation,
            wire.mapping_artifact_count,
            wire.state,
        )
        .map_err(serde::de::Error::custom)
    }
}
impl StagedSpaceArtifact {
    pub fn new(
        space: NamedVectorSpaceId,
        generation: PublicationGeneration,
        mapping_artifact_count: u64,
        state: SpacePublicationState,
    ) -> NamedVectorSpaceResult<Self> {
        if mapping_artifact_count == 0 && state == SpacePublicationState::Complete {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidPublicationManifest,
            ));
        }
        Ok(Self {
            space,
            generation,
            mapping_artifact_count,
            state,
        })
    }
    pub const fn space(&self) -> &NamedVectorSpaceId {
        &self.space
    }
    pub const fn generation(&self) -> PublicationGeneration {
        self.generation
    }
    pub const fn state(&self) -> SpacePublicationState {
        self.state
    }
}

/// Atomically publishable manifest. It lists complete, optional, unavailable and
/// stale spaces instead of letting callers infer partial state from absence.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct NamedVectorPublicationManifest {
    generation: PublicationGeneration,
    spaces: Vec<StagedSpaceArtifact>,
}
impl<'de> Deserialize<'de> for NamedVectorPublicationManifest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            generation: PublicationGeneration,
            spaces: Vec<StagedSpaceArtifact>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.generation, wire.spaces).map_err(serde::de::Error::custom)
    }
}
impl NamedVectorPublicationManifest {
    pub fn new(
        generation: PublicationGeneration,
        spaces: Vec<StagedSpaceArtifact>,
    ) -> NamedVectorSpaceResult<Self> {
        if spaces.is_empty() {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::InvalidPublicationManifest,
            ));
        }
        let mut seen = BTreeSet::new();
        for space in &spaces {
            if space.generation != generation || !seen.insert(space.space.as_str()) {
                return Err(NamedVectorSpaceError::contract(
                    NamedVectorSpaceDiagnosticCode::InvalidPublicationManifest,
                ));
            }
        }
        Ok(Self { generation, spaces })
    }
    pub const fn generation(&self) -> PublicationGeneration {
        self.generation
    }
    pub fn spaces(&self) -> &[StagedSpaceArtifact] {
        &self.spaces
    }
}

/// Versioned source operation. A tombstone denotes removal of every derived
/// representation; there is no API for deleting only an untracked subset.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "operation")]
pub enum DerivedRepresentationOperation {
    Replace { generation: PublicationGeneration },
    TombstoneAll { generation: PublicationGeneration },
}

/// Independent retention request. Deleting a space while evidence remains
/// referenced is rejected rather than silently breaking hydrated results.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SpaceRetentionRequest {
    space: NamedVectorSpaceId,
    referenced_evidence_count: u64,
    delete_space: bool,
}

impl<'de> Deserialize<'de> for SpaceRetentionRequest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            space: NamedVectorSpaceId,
            referenced_evidence_count: u64,
            delete_space: bool,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.space,
            wire.referenced_evidence_count,
            wire.delete_space,
        )
        .map_err(serde::de::Error::custom)
    }
}
impl SpaceRetentionRequest {
    pub fn new(
        space: NamedVectorSpaceId,
        referenced_evidence_count: u64,
        delete_space: bool,
    ) -> NamedVectorSpaceResult<Self> {
        if delete_space && referenced_evidence_count > 0 {
            return Err(NamedVectorSpaceError::contract(
                NamedVectorSpaceDiagnosticCode::ReferencedEvidenceRetention,
            ));
        }
        Ok(Self {
            space,
            referenced_evidence_count,
            delete_space,
        })
    }
    pub const fn delete_space(&self) -> bool {
        self.delete_space
    }
}
