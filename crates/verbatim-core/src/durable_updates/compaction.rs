//! Measured-trigger compaction, resumable plans, and generation/query leases.
//!
//! Compaction is triggered by measured dead-byte ratio, read amplification, update
//! volume, or latency degradation — never by wall-clock time alone. A
//! [`CompactionPlan`] is resumable and restart-safe: it records staged progress so
//! a crashed compaction resumes or quarantines rather than corrupting the live
//! generation. It produces a staged immutable artifact. Old pages are reclaimed
//! only after their [`MutationLease`] expires, protecting in-flight queries bound
//! to a previous generation.

use serde::{Deserialize, Serialize};

use super::identity::DurableGeneration;
use super::{DurableUpdateDiagnosticCode, DurableUpdateError, DurableUpdateResult};

/// Measured signals that may trigger compaction. At least one must exceed its
/// threshold; time alone is insufficient.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CompactionTrigger {
    /// Fraction of dead (tombstoned or overwritten) bytes in the live shard,
    /// in `0.0..=1.0`.
    dead_byte_ratio: f64,
    /// Read amplification: average SSD pages read per candidate returned.
    read_amplification: f64,
    /// Number of mutations applied since the last compaction.
    update_volume: u64,
    /// p99 query latency in microseconds observed since the last compaction.
    p99_latency_us: f64,
    /// Thresholds above which the corresponding signal triggers compaction.
    thresholds: CompactionThresholds,
}

/// Configured thresholds for each compaction signal.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CompactionThresholds {
    pub dead_byte_ratio: f64,
    pub read_amplification: f64,
    pub update_volume: u64,
    pub p99_latency_us: f64,
}

impl CompactionThresholds {
    /// Conservative default thresholds. A future operator config may override these.
    pub const DEFAULT: Self = Self {
        dead_byte_ratio: 0.20,
        read_amplification: 4.0,
        update_volume: 50_000,
        p99_latency_us: 50_000.0,
    };

    /// Validates that every threshold is finite and positive.
    pub fn validate(&self) -> DurableUpdateResult<()> {
        let ratios = [
            self.dead_byte_ratio,
            self.read_amplification,
            self.p99_latency_us,
        ];
        for value in ratios {
            if !value.is_finite() || value <= 0.0 {
                return Err(DurableUpdateError::contract(
                    DurableUpdateDiagnosticCode::InvalidCompactionPlan,
                ));
            }
        }
        if self.dead_byte_ratio > 1.0 {
            return Err(DurableUpdateError::contract(
                DurableUpdateDiagnosticCode::InvalidCompactionPlan,
            ));
        }
        Ok(())
    }
}

impl Default for CompactionThresholds {
    fn default() -> Self {
        Self::DEFAULT
    }
}

impl CompactionTrigger {
    /// Constructs a measured trigger snapshot with explicit thresholds.
    pub fn new(
        dead_byte_ratio: f64,
        read_amplification: f64,
        update_volume: u64,
        p99_latency_us: f64,
        thresholds: CompactionThresholds,
    ) -> DurableUpdateResult<Self> {
        thresholds.validate()?;
        for value in [dead_byte_ratio, read_amplification, p99_latency_us] {
            if !value.is_finite() || value < 0.0 {
                return Err(DurableUpdateError::contract(
                    DurableUpdateDiagnosticCode::InvalidCompactionPlan,
                ));
            }
        }
        if dead_byte_ratio > 1.0 {
            return Err(DurableUpdateError::contract(
                DurableUpdateDiagnosticCode::InvalidCompactionPlan,
            ));
        }
        Ok(Self {
            dead_byte_ratio,
            read_amplification,
            update_volume,
            p99_latency_us,
            thresholds,
        })
    }

    /// Returns `true` if at least one measured signal exceeds its threshold.
    /// Time alone never triggers compaction.
    pub fn should_compact(&self) -> bool {
        self.dead_byte_ratio >= self.thresholds.dead_byte_ratio
            || self.read_amplification >= self.thresholds.read_amplification
            || self.update_volume >= self.thresholds.update_volume
            || self.p99_latency_us >= self.thresholds.p99_latency_us
    }

    /// Returns the measured dead-byte ratio.
    pub const fn dead_byte_ratio(&self) -> f64 {
        self.dead_byte_ratio
    }

    /// Returns the measured read amplification.
    pub const fn read_amplification(&self) -> f64 {
        self.read_amplification
    }

    /// Returns the measured update volume.
    pub const fn update_volume(&self) -> u64 {
        self.update_volume
    }

    /// Returns the measured p99 latency in microseconds.
    pub const fn p99_latency_us(&self) -> f64 {
        self.p99_latency_us
    }
}

/// Resumable stage of a compaction run.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompactionStage {
    /// Compaction requested; waiting for a resource budget slot.
    Pending,
    /// Streaming live + tombstone-free vectors into the staged artifact.
    Streaming,
    /// Rebuilding graph edges in the compacted artifact.
    RebuildingGraph,
    /// Validating recall, connectivity, and filter coverage.
    Validating,
    /// Staged immutable artifact ready for atomic publication.
    Staged,
    /// Old pages reclaimed after lease expiry; compaction complete.
    Complete,
}

impl CompactionStage {
    /// Returns `true` once the staged immutable artifact is durable.
    pub const fn is_staged(self) -> bool {
        matches!(self, Self::Staged | Self::Complete)
    }

    /// Returns `true` only for the terminal, fully-reclaimed stage.
    pub const fn is_complete(self) -> bool {
        matches!(self, Self::Complete)
    }
}

/// A resumable, restart-safe compaction plan.
///
/// The plan records the source generation being compacted, the target generation
/// that will receive the staged artifact, the current stage, and whether the
/// staged artifact has been fsynced. On recovery, a plan that is not at `Staged`
/// or `Complete` resumes from its last durable stage; a plan whose staged
/// artifact is not fsynced is quarantined.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompactionPlan {
    source_generation: DurableGeneration,
    target_generation: DurableGeneration,
    stage: CompactionStage,
    staged_artifact_fsynced: bool,
}

impl CompactionPlan {
    /// Constructs a compaction plan after validating generation ordering.
    pub fn new(
        source_generation: DurableGeneration,
        target_generation: DurableGeneration,
        stage: CompactionStage,
        staged_artifact_fsynced: bool,
    ) -> DurableUpdateResult<Self> {
        if target_generation.value() <= source_generation.value() {
            return Err(DurableUpdateError::contract(
                DurableUpdateDiagnosticCode::InvalidCompactionPlan,
            ));
        }
        if stage.is_staged() && !staged_artifact_fsynced {
            return Err(DurableUpdateError::contract(
                DurableUpdateDiagnosticCode::CheckpointNotDurable,
            ));
        }
        Ok(Self {
            source_generation,
            target_generation,
            stage,
            staged_artifact_fsynced,
        })
    }

    /// Returns the generation being compacted away.
    pub const fn source_generation(&self) -> DurableGeneration {
        self.source_generation
    }

    /// Returns the generation that will receive the staged artifact.
    pub const fn target_generation(&self) -> DurableGeneration {
        self.target_generation
    }

    /// Returns the current resumable stage.
    pub const fn stage(&self) -> CompactionStage {
        self.stage
    }

    /// Returns whether the staged artifact has been fsynced.
    pub const fn staged_artifact_fsynced(&self) -> bool {
        self.staged_artifact_fsynced
    }

    /// Returns `true` if the staged immutable artifact is durable and ready.
    pub fn is_ready_to_publish(&self) -> bool {
        self.stage == CompactionStage::Staged && self.staged_artifact_fsynced
    }
}

/// A generation or query lease governing when old pages may be reclaimed.
///
/// After a new generation is published, the previous generation's pages remain
/// readable until every in-flight query lease expires. Reclamation before lease
/// expiry is rejected. A lease records the generation it protects and an expiry
/// sequence number; leases are comparable so the earliest-expiring lease can be
/// drained first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct MutationLease {
    generation: DurableGeneration,
    expiry_seq: u64,
}

impl MutationLease {
    /// Constructs a lease protecting `generation` until sequence `expiry_seq`.
    pub const fn new(generation: DurableGeneration, expiry_seq: u64) -> Self {
        Self {
            generation,
            expiry_seq,
        }
    }

    /// Returns the generation this lease protects from reclamation.
    pub const fn generation(&self) -> DurableGeneration {
        self.generation
    }

    /// Returns the expiry sequence number.
    pub const fn expiry_seq(&self) -> u64 {
        self.expiry_seq
    }

    /// Returns `true` if `now_seq` has passed this lease's expiry.
    pub const fn is_expired(&self, now_seq: u64) -> bool {
        now_seq >= self.expiry_seq
    }
}

/// Decides whether old pages for a generation may be reclaimed, given the set of
/// active leases. Reclamation is rejected if any live lease still protects the
/// generation.
pub fn can_reclaim_generation(
    generation: DurableGeneration,
    leases: &[MutationLease],
    now_seq: u64,
) -> DurableUpdateResult<()> {
    for lease in leases {
        if lease.generation() == generation && !lease.is_expired(now_seq) {
            return Err(DurableUpdateError::contract(
                DurableUpdateDiagnosticCode::LeaseActive,
            ));
        }
    }
    Ok(())
}
