//! Atomic active-generation pointer, lease tracking, and lifecycle transitions
//! for publication coordination (Refs #379).
//!
//! The pointer is the atomic CAS boundary for promotion and rollback. A query
//! binds to exactly one generation via the pointer; only `Active` generations
//! serve queries. Leases prevent old-generation GC while cursors or in-flight
//! queries still reference a retained generation.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use super::error::{
    GenerationPublicationDiagnosticCode, GenerationPublicationError, GenerationPublicationResult,
};
use super::identity::{CoordinatorEpoch, PublicationGenerationId};
use super::manifest::PublicationStage;

/// Atomic active-generation pointer: the single source of truth for which
/// generation serves queries.
///
/// Promotion and rollback are CAS operations on `(active_generation, epoch)`.
/// The previous generation is retained for rollback until its leases expire.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PublicationPointer {
    pub active_generation: PublicationGenerationId,
    pub epoch: CoordinatorEpoch,
    /// Generation promoted from (for one-step rollback).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_generation: Option<PublicationGenerationId>,
    /// Wall-clock timestamp of the last promotion (RFC3339 or adapter-defined).
    pub updated_at: String,
}

impl PublicationPointer {
    /// Constructs a validated pointer.
    pub fn new(
        generation: PublicationGenerationId,
        epoch: CoordinatorEpoch,
        updated_at: impl Into<String>,
    ) -> GenerationPublicationResult<Self> {
        let updated_at = updated_at.into();
        if updated_at.trim().is_empty() {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            ));
        }
        Ok(Self {
            active_generation: generation,
            epoch,
            previous_generation: None,
            updated_at,
        })
    }

    /// Attaches a previous generation for rollback.
    pub fn with_previous(mut self, generation: PublicationGenerationId) -> Self {
        self.previous_generation = Some(generation);
        self
    }

    /// Validates pointer consistency: epoch nonzero, updated_at non-empty.
    pub fn validate(&self) -> GenerationPublicationResult<()> {
        if self.updated_at.trim().is_empty() {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            ));
        }
        // Epoch/generation nonzero is enforced by their constructors.
        Ok(())
    }
}

/// One generation lease: a cursor or in-flight query holding a retained
/// generation readable until expiry or explicit release.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GenerationLease {
    pub generation: PublicationGenerationId,
    /// Monotonic lease id (caller-supplied, e.g. cursor id).
    pub lease_id: u64,
    /// Absolute expiry as a monotonic wall-clock value (adapter-defined unit).
    pub expires_at: u64,
}

impl GenerationLease {
    /// Constructs a lease. `expires_at` must be nonzero.
    pub fn new(
        generation: PublicationGenerationId,
        lease_id: u64,
        expires_at: u64,
    ) -> GenerationPublicationResult<Self> {
        if lease_id == 0 || expires_at == 0 {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(Self {
            generation,
            lease_id,
            expires_at,
        })
    }

    /// Returns `true` if the lease has expired relative to `now`.
    pub const fn is_expired(&self, now: u64) -> bool {
        now >= self.expires_at
    }
}

/// In-memory lease registry tracking outstanding leases per generation.
///
/// A generation is reclaimable only when it has no outstanding, non-expired
/// leases. Leases are released explicitly or pruned by expiry.
#[derive(Debug)]
pub struct LeaseRegistry {
    leases: HashMap<u64, Vec<GenerationLease>>,
    lock: Mutex<()>,
}

impl Default for LeaseRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl LeaseRegistry {
    /// Constructs an empty lease registry.
    pub fn new() -> Self {
        Self {
            leases: HashMap::new(),
            lock: Mutex::new(()),
        }
    }

    /// Acquires a lease for a generation.
    pub fn acquire(&mut self, lease: GenerationLease) -> GenerationPublicationResult<()> {
        let _guard = self.lock.lock().map_err(|_| {
            GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            )
        })?;
        let entry = self.leases.entry(lease.generation.value()).or_default();
        if entry.iter().any(|l| l.lease_id == lease.lease_id) {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidIdentity,
            ));
        }
        entry.push(lease);
        Ok(())
    }

    /// Releases a specific lease by id.
    pub fn release(
        &mut self,
        generation: PublicationGenerationId,
        lease_id: u64,
    ) -> GenerationPublicationResult<()> {
        let _guard = self.lock.lock().map_err(|_| {
            GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            )
        })?;
        let entry = self.leases.get_mut(&generation.value()).ok_or_else(|| {
            GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidIdentity,
            )
        })?;
        let len_before = entry.len();
        entry.retain(|l| l.lease_id != lease_id);
        if entry.len() == len_before {
            return Err(GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(())
    }

    /// Returns `true` if the generation has any non-expired leases.
    pub fn has_active_leases(
        &self,
        generation: PublicationGenerationId,
        now: u64,
    ) -> GenerationPublicationResult<bool> {
        let _guard = self.lock.lock().map_err(|_| {
            GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            )
        })?;
        Ok(self
            .leases
            .get(&generation.value())
            .is_some_and(|leases| leases.iter().any(|l| !l.is_expired(now))))
    }

    /// Prunes expired leases for all generations and returns how many were
    /// removed.
    pub fn prune_expired(&mut self, now: u64) -> GenerationPublicationResult<usize> {
        let _guard = self.lock.lock().map_err(|_| {
            GenerationPublicationError::contract(
                GenerationPublicationDiagnosticCode::InvalidContract,
            )
        })?;
        let mut pruned = 0usize;
        for leases in self.leases.values_mut() {
            let len_before = leases.len();
            leases.retain(|l| !l.is_expired(now));
            pruned += len_before - leases.len();
        }
        Ok(pruned)
    }
}

/// Validated lifecycle transition for a generation stage.
///
/// Enforces the issue #379 lifecycle:
/// `SnapshotFixed → Staging → Validating → Ready → Active → Retained
///  → GarbageCollected`, plus `Validating → Quarantined` and
/// `Active → Retained` / `Retained → Quarantined`.
pub fn validate_stage_transition(
    from: PublicationStage,
    to: PublicationStage,
) -> GenerationPublicationResult<()> {
    use PublicationStage::*;
    let allowed = matches!(
        (from, to),
        (SnapshotFixed, Staging)
            | (Staging, Validating)
            | (Validating, Ready)
            | (Validating, Quarantined)
            | (Ready, Active)
            | (Ready, Quarantined)
            | (Active, Retained)
            | (Retained, GarbageCollected)
            | (Retained, Quarantined)
    );
    if !allowed {
        return Err(GenerationPublicationError::contract(
            GenerationPublicationDiagnosticCode::InvalidStageTransition,
        ));
    }
    Ok(())
}

/// Returns `true` if a generation at `stage` may be promoted to active.
pub fn can_promote(stage: PublicationStage) -> bool {
    stage.can_promote()
}
