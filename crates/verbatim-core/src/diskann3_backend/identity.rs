//! Stable index IDs and versioned mappings back to authoritative Verbatim chunk IDs.

use std::collections::BTreeMap;

use crate::diskann3::{PublicationGeneration, VectorSpaceId};
use crate::types::{ChunkId, EmbeddingProfileId};

use super::{
    DiskAnnBackendDiagnosticCode, DiskAnnBackendError, DiskAnnBackendResult, VectorSpaceSpec,
};

/// Stable positive numeric ID stored in a DiskANN index generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct StableVectorId(u64);

impl StableVectorId {
    /// Constructs a nonzero ID that remains stable within the mapping history.
    pub fn new(value: u64) -> DiskAnnBackendResult<Self> {
        if value == 0 {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidStableVectorId,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the serialized numeric ID.
    pub const fn value(self) -> u64 {
        self.0
    }
}

/// Positive schema/version number for the index-to-chunk mapping envelope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct MappingVersion(u32);

impl MappingVersion {
    /// Constructs a nonzero versioned mapping envelope.
    pub fn new(value: u32) -> DiskAnnBackendResult<Self> {
        if value == 0 {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidMappingVersion,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the mapping-envelope version.
    pub const fn value(self) -> u32 {
        self.0
    }
}

/// One stable index-ID to authoritative chunk-ID relation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkIdMappingEntry {
    vector_id: StableVectorId,
    chunk_id: ChunkId,
}

impl ChunkIdMappingEntry {
    /// Creates an entry whose chunk data remains authoritative outside the index files.
    pub fn new(vector_id: StableVectorId, chunk_id: ChunkId) -> Self {
        Self {
            vector_id,
            chunk_id,
        }
    }
}

/// Versioned mapping required to resolve index hits without placing text or ACLs in the index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChunkIdMapping {
    version: MappingVersion,
    vector_space_id: VectorSpaceId,
    profile_id: EmbeddingProfileId,
    generation: PublicationGeneration,
    entries: BTreeMap<StableVectorId, ChunkId>,
}

impl ChunkIdMapping {
    /// Builds a mapping after validating its identity envelope and rejecting duplicate IDs.
    pub fn new(
        version: MappingVersion,
        vector_space_id: VectorSpaceId,
        profile_id: EmbeddingProfileId,
        generation: PublicationGeneration,
        entries: Vec<ChunkIdMappingEntry>,
    ) -> DiskAnnBackendResult<Self> {
        vector_space_id.validate().map_err(|_| {
            DiskAnnBackendError::contract(DiskAnnBackendDiagnosticCode::InvalidChunkIdMapping)
        })?;
        if generation.value() == 0 {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidChunkIdMapping,
            ));
        }

        let mut by_vector_id = BTreeMap::new();
        for entry in entries {
            if entry.chunk_id.0.trim().is_empty() {
                return Err(DiskAnnBackendError::contract(
                    DiskAnnBackendDiagnosticCode::InvalidChunkIdMapping,
                ));
            }
            if by_vector_id
                .insert(entry.vector_id, entry.chunk_id)
                .is_some()
            {
                return Err(DiskAnnBackendError::contract(
                    DiskAnnBackendDiagnosticCode::DuplicateStableVectorId,
                ));
            }
        }

        Ok(Self {
            version,
            vector_space_id,
            profile_id,
            generation,
            entries: by_vector_id,
        })
    }

    /// Returns the serialized mapping-envelope version.
    pub const fn version(&self) -> MappingVersion {
        self.version
    }

    /// Returns the generation protected by this mapping.
    pub const fn generation(&self) -> PublicationGeneration {
        self.generation
    }

    /// Looks up an authoritative chunk ID for an index-stable vector ID.
    pub fn chunk_id(&self, vector_id: StableVectorId) -> Option<&ChunkId> {
        self.entries.get(&vector_id)
    }

    /// Rejects using a mapping with another vector space, profile, or publication generation.
    pub fn validate_binding(
        &self,
        vector_space: &VectorSpaceSpec,
        generation: PublicationGeneration,
    ) -> DiskAnnBackendResult<()> {
        if &self.vector_space_id != vector_space.vector_space_id() {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::VectorSpaceMismatch,
            ));
        }
        if &self.profile_id != vector_space.profile_id() {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::ProfileMismatch,
            ));
        }
        if self.generation != generation {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::GenerationMismatch,
            ));
        }
        Ok(())
    }
}
