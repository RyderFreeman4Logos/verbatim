//! Dual-generation migration evaluation, coordinator exclusivity, quarantine, and
//! rollback durability (Refs #379).
//!
//! This module defines the migration contract for no-downtime cutover between
//! vector backends (SQLite→DiskANN3, standard→AISAQ, version/layout upgrades,
//! Qdrant/LanceDB fallback, embedding-profile changes). Two candidate
//! generations are evaluated under mirrored sampled queries with independent
//! metrics; fusion of old/new backend results is never the default.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::error::{
    GenerationPublicationDiagnosticCode, GenerationPublicationError, GenerationPublicationResult,
};
use super::identity::{CoordinatorEpoch, PublicationGenerationId};
use super::manifest::VectorBackendProvider;

/// One coordinator lock: prevents two coordinators from promoting different
/// generations concurrently.
///
/// The lock is held under a coordinator id and epoch. A promotion may proceed
/// only if the lock is either free or held by the same coordinator at the same
/// epoch. A rollback releases the lock (durable rollback receipt).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoordinatorLock {
    /// Coordinator id holding the lock (opaque, caller-supplied).
    pub coordinator_id: u64,
    pub epoch: CoordinatorEpoch,
    /// Generation the lock was acquired to promote.
    pub target_generation: PublicationGenerationId,
}

impl CoordinatorLock {
    /// Constructs a validated lock entry.
    pub fn new(
        coordinator_id: u64,
        epoch: CoordinatorEpoch,
        target_generation: PublicationGenerationId,
    ) -> GenerationPublicationResult<Self> {
        if coordinator_id == 0 {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(Self {
            coordinator_id,
            epoch,
            target_generation,
        })
    }
}

/// In-memory coordinator-lock registry enforcing promotion exclusivity.
///
/// Only one generation may be promoted at a time. Acquiring a lock for a
/// different generation while another is held fails with `CoordinatorLocked`.
#[derive(Debug)]
pub struct CoordinatorLockRegistry {
    current: Option<CoordinatorLock>,
    lock: Mutex<()>,
}

impl Default for CoordinatorLockRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl CoordinatorLockRegistry {
    /// Constructs an empty lock registry.
    pub fn new() -> Self {
        Self {
            current: None,
            lock: Mutex::new(()),
        }
    }

    /// Attempts to acquire a coordinator lock. Fails if a different target
    /// generation is already locked.
    pub fn acquire(&mut self, entry: CoordinatorLock) -> GenerationPublicationResult<()> {
        let _guard = self.lock.lock().map_err(|_| {
            GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            )
        })?;
        if let Some(existing) = &self.current {
            if existing.target_generation != entry.target_generation {
                return Err(GenerationPublicationError::contract(
                    GenerationPublicationDiagnosticCode::CoordinatorLocked,
                ));
            }
            // Same target: re-acquire by the same coordinator is allowed; a
            // different coordinator on the same target is rejected.
            if existing.coordinator_id != entry.coordinator_id {
                return Err(GenerationPublicationError::contract(
                    GenerationPublicationDiagnosticCode::CoordinatorLocked,
                ));
            }
        }
        self.current = Some(entry);
        Ok(())
    }

    /// Releases the lock if held by the given coordinator at the given epoch.
    pub fn release(
        &mut self,
        coordinator_id: u64,
        epoch: CoordinatorEpoch,
    ) -> GenerationPublicationResult<()> {
        let _guard = self.lock.lock().map_err(|_| {
            GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            )
        })?;
        match &self.current {
            Some(existing)
                if existing.coordinator_id == coordinator_id && existing.epoch == epoch =>
            {
                self.current = None;
                Ok(())
            }
            _ => Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::CoordinatorLocked,
            )),
        }
    }

    /// Returns the current lock, if any.
    pub fn current(&self) -> Option<&CoordinatorLock> {
        self.current.as_ref()
    }
}

/// Independent metrics captured for one candidate backend under mirrored
/// sampled queries.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct MigrationCandidateMetrics {
    pub recall_at_10: f64,
    /// p99 query latency in microseconds.
    pub p99_latency_us: f64,
    /// Peak memory in bytes.
    pub peak_memory_bytes: u64,
    /// SSD read amplification (pages per candidate).
    pub read_amplification: f64,
}

impl MigrationCandidateMetrics {
    pub fn validate(&self) -> GenerationPublicationResult<()> {
        if !(0.0..=1.0).contains(&self.recall_at_10)
            || self.p99_latency_us <= 0.0
            || self.peak_memory_bytes == 0
            || self.read_amplification <= 0.0
        {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidBounds,
            ));
        }
        Ok(())
    }
}

/// Whether and how old/new backend results are combined during a migration
/// evaluation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FusionPolicy {
    /// Default: no fusion. Old and new results are never mixed in one response.
    None,
    /// Explicit experiment profile opt-in. Fusion logic is defined by the
    /// experiment; this contract only records the opt-in.
    Experiment,
}

/// Dual-generation migration evaluation profile.
///
/// Mirrors sampled queries against the incumbent and candidate generation,
/// persisting independent metrics. By default (`FusionPolicy::None`), results
/// are never fused — the evaluation is a shadow comparison only.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MigrationProfile {
    /// Incumbent (current active) generation under evaluation.
    pub incumbent_generation: PublicationGenerationId,
    /// Candidate generation being shadowed.
    pub candidate_generation: PublicationGenerationId,
    /// Incumbent backend provider.
    pub incumbent_backend: VectorBackendProvider,
    /// Candidate backend provider.
    pub candidate_backend: VectorBackendProvider,
    /// Number of mirrored sampled queries.
    pub sample_size: u32,
    pub fusion_policy: FusionPolicy,
    pub incumbent_metrics: MigrationCandidateMetrics,
    pub candidate_metrics: MigrationCandidateMetrics,
}

impl MigrationProfile {
    /// Validates the migration profile.
    pub fn validate(&self) -> GenerationPublicationResult<()> {
        if self.incumbent_generation == self.candidate_generation {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            ));
        }
        if self.sample_size == 0 {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidBounds,
            ));
        }
        self.incumbent_metrics.validate()?;
        self.candidate_metrics.validate()?;
        Ok(())
    }

    /// Returns `true` if the candidate's recall meets or exceeds the incumbent.
    pub fn candidate_recall_meets_or_exceeds_incumbent(&self) -> bool {
        self.candidate_metrics.recall_at_10 >= self.incumbent_metrics.recall_at_10
    }

    /// Returns `true` if results are fused by default (they are not).
    pub const fn fuses_by_default(&self) -> bool {
        matches!(self.fusion_policy, FusionPolicy::Experiment)
    }
}

/// Quarantine record for an incomplete or corrupt generation isolated after
/// startup reconciliation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuarantineRecord {
    pub generation: PublicationGenerationId,
    /// Wall-clock quarantine timestamp.
    pub quarantined_at: String,
    /// Diagnostic code describing why the generation was quarantined.
    pub reason: GenerationPublicationDiagnosticCode,
}

impl QuarantineRecord {
    /// Constructs a validated quarantine record.
    pub fn new(
        generation: PublicationGenerationId,
        quarantined_at: impl Into<String>,
        reason: GenerationPublicationDiagnosticCode,
    ) -> GenerationPublicationResult<Self> {
        let quarantined_at = quarantined_at.into();
        if quarantined_at.trim().is_empty() {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            ));
        }
        Ok(Self {
            generation,
            quarantined_at,
            reason,
        })
    }

    /// Returns `true` if the quarantine conflicts with a later promotion or ACL
    /// generation. A quarantined generation can never be promoted under a newer
    /// evidence/ACL generation.
    pub fn conflicts_with_newer_generation(
        &self,
        newer_generation: PublicationGenerationId,
    ) -> bool {
        self.generation.value() < newer_generation.value()
    }
}

/// Quarantine registry keyed by generation.
#[derive(Debug, Default)]
pub struct QuarantineRegistry {
    records: HashMap<u64, QuarantineRecord>,
    lock: Mutex<()>,
}

impl QuarantineRegistry {
    /// Constructs an empty quarantine registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Quarantines a generation.
    pub fn quarantine(&mut self, record: QuarantineRecord) -> GenerationPublicationResult<()> {
        let _guard = self.lock.lock().map_err(|_| {
            GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            )
        })?;
        if self.records.contains_key(&record.generation.value()) {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::QuarantineConflict,
            ));
        }
        self.records.insert(record.generation.value(), record);
        Ok(())
    }

    /// Returns `true` if the generation is quarantined.
    pub fn is_quarantined(
        &self,
        generation: PublicationGenerationId,
    ) -> GenerationPublicationResult<bool> {
        let _guard = self.lock.lock().map_err(|_| {
            GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            )
        })?;
        Ok(self.records.contains_key(&generation.value()))
    }

    /// Returns the quarantine record for a generation, if any.
    pub fn get(
        &self,
        generation: PublicationGenerationId,
    ) -> GenerationPublicationResult<Option<&QuarantineRecord>> {
        let _guard = self.lock.lock().map_err(|_| {
            GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            )
        })?;
        Ok(self.records.get(&generation.value()))
    }
}

/// Durable fsync attestation for a rollback receipt.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackFsyncAttestation {
    data_fsynced: bool,
    dir_fsynced: bool,
}

impl RollbackFsyncAttestation {
    /// Constructs an attestation.
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

/// Rollback receipt proving the rollback was durable across restart.
///
/// A rollback that was not fully fsynced is rejected as not durable
/// (`RollbackNotDurable`); the previous active generation remains live.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollbackReceipt {
    pub demoted_generation: PublicationGenerationId,
    pub restored_generation: PublicationGenerationId,
    pub epoch: CoordinatorEpoch,
    pub fsync: RollbackFsyncAttestation,
    pub rolled_back_at: String,
}

impl RollbackReceipt {
    /// Constructs and validates a rollback receipt.
    pub fn new(
        demoted_generation: PublicationGenerationId,
        restored_generation: PublicationGenerationId,
        epoch: CoordinatorEpoch,
        fsync: RollbackFsyncAttestation,
        rolled_back_at: impl Into<String>,
    ) -> GenerationPublicationResult<Self> {
        let rolled_back_at = rolled_back_at.into();
        if rolled_back_at.trim().is_empty() {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            ));
        }
        if demoted_generation == restored_generation {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            ));
        }
        let receipt = Self {
            demoted_generation,
            restored_generation,
            epoch,
            fsync,
            rolled_back_at,
        };
        receipt.validate_durability()?;
        Ok(receipt)
    }

    /// Returns `Ok(())` only if the rollback is fully durable across restart.
    pub fn validate_durability(&self) -> GenerationPublicationResult<()> {
        if !self.fsync.is_fully_durable() {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::RollbackNotDurable,
            ));
        }
        Ok(())
    }

    /// Returns `true` if the receipt proves the rollback survived a restart.
    pub const fn is_durable_across_restart(&self) -> bool {
        self.fsync.is_fully_durable()
    }
}

/// Validates that two generations are never read together in a single query
/// (mixed-index read). A query must bind to exactly one publication generation.
pub fn reject_mixed_generation_read(
    a: PublicationGenerationId,
    b: PublicationGenerationId,
) -> GenerationPublicationResult<()> {
    if a != b {
        return Err(GenerationPublicationError::contract(
            GenerationPublicationDiagnosticCode::MixedGenerationRead,
        ));
    }
    Ok(())
}
