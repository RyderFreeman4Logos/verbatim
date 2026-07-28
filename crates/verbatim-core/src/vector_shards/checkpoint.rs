//! Resumable, crash-safe build checkpoints with fsync attestation.
//!
//! Shard builds run in a separate process/cgroup from online serving. Builds use
//! bounded streaming batches and resumable checkpoints. Before a shard is marked
//! complete, the build must `fsync` both the data files and the directory
//! metadata. A checkpoint records the durable attestation so recovery can resume
//! or quarantine after a crash at any point.

use serde::{Deserialize, Serialize};

use super::identity::{ShardGeneration, ShardId};
use super::{VectorShardDiagnosticCode, VectorShardError, VectorShardResult};

/// Stage of a bounded, resumable shard build.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ShardBuildStage {
    /// Streaming vectors and graph pages in bounded batches.
    StreamingData,
    /// Writing candidate codes and the id-map.
    WritingMetadata,
    /// fsync of data files and directory metadata before marking complete.
    Fsyncing,
    /// Validating file hashes, dimensions, ID referential integrity, filter
    /// coverage, and sampled recall.
    Validating,
    /// Build complete and durable; ready for publication through #379.
    Complete,
}

impl ShardBuildStage {
    /// Returns `true` once the build has passed fsync attestation.
    pub const fn is_durable(self) -> bool {
        matches!(self, Self::Fsyncing | Self::Validating | Self::Complete)
    }

    /// Returns `true` only for the terminal, published-ready stage.
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// Durable fsync attestation recorded by a checkpoint.
///
/// `data_fsynced` attests that shard data files were fsynced. `dir_fsynced`
/// attests that the directory metadata was fsynced so the files are durably
/// linked. A checkpoint may not reach `Complete` until both are true.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct FsyncAttestation {
    data_fsynced: bool,
    dir_fsynced: bool,
}

impl FsyncAttestation {
    /// Constructs an attestation. `Complete` requires both fields to be `true`.
    pub const fn new(data_fsynced: bool, dir_fsynced: bool) -> Self {
        Self {
            data_fsynced,
            dir_fsynced,
        }
    }

    /// Returns whether data files were fsynced.
    pub const fn data_fsynced(self) -> bool {
        self.data_fsynced
    }

    /// Returns whether directory metadata was fsynced.
    pub const fn dir_fsynced(self) -> bool {
        self.dir_fsynced
    }

    /// Returns `true` only when both data and directory are durably fsynced.
    pub const fn is_fully_durable(self) -> bool {
        self.data_fsynced && self.dir_fsynced
    }
}

/// A resumable checkpoint in a bounded shard build.
///
/// The checkpoint records the build stage, the number of vectors streamed so far,
/// and the fsync attestation. On recovery, the builder resumes from the last
/// durable checkpoint or quarantines a partially-written shard that cannot be
/// resumed. A checkpoint at `Complete` is rejected unless the build is fully
/// durable.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ShardBuildCheckpoint {
    shard: ShardId,
    generation: ShardGeneration,
    stage: ShardBuildStage,
    vectors_streamed: u64,
    fsync: FsyncAttestation,
}

impl ShardBuildCheckpoint {
    /// Constructs a checkpoint after validating identity, progress, and durability.
    pub fn new(
        shard: ShardId,
        generation: ShardGeneration,
        stage: ShardBuildStage,
        vectors_streamed: u64,
        fsync: FsyncAttestation,
    ) -> VectorShardResult<Self> {
        let checkpoint = Self {
            shard,
            generation,
            stage,
            vectors_streamed,
            fsync,
        };
        checkpoint.validate()?;
        Ok(checkpoint)
    }

    /// Revalidates the checkpoint contract.
    ///
    /// - The shard generation must match the checkpoint generation.
    /// - Progress (`vectors_streamed`) may be zero only before data streaming
    ///   begins; once past `StreamingData` it must be positive.
    /// - A `Complete` checkpoint requires full fsync durability.
    pub fn validate(&self) -> VectorShardResult<()> {
        self.shard.validate()?;
        if self.generation != self.shard.generation() {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidCheckpoint,
            ));
        }
        if self.stage != ShardBuildStage::StreamingData && self.vectors_streamed == 0 {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::InvalidCheckpoint,
            ));
        }
        // A stage at or past fsync requires the attestation to be fully durable,
        // since the checkpoint asserts that data + dir were fsynced before
        // advancing.
        if self.stage.is_durable() && !self.fsync.is_fully_durable() {
            return Err(VectorShardError::contract(
                VectorShardDiagnosticCode::CheckpointNotDurable,
            ));
        }
        Ok(())
    }

    /// Returns the shard being built.
    pub const fn shard(&self) -> &ShardId {
        &self.shard
    }

    /// Returns the generation under construction.
    pub const fn generation(&self) -> ShardGeneration {
        self.generation
    }

    /// Returns the current build stage.
    pub const fn stage(&self) -> ShardBuildStage {
        self.stage
    }

    /// Returns the number of vectors streamed so far.
    pub const fn vectors_streamed(&self) -> u64 {
        self.vectors_streamed
    }

    /// Returns the fsync attestation.
    pub const fn fsync(&self) -> FsyncAttestation {
        self.fsync
    }

    /// Returns `true` if the checkpoint is at the terminal published-ready stage.
    pub const fn is_complete(&self) -> bool {
        self.stage.is_complete()
    }
}
