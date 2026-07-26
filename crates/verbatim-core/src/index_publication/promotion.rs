//! Promotion / rollback state machine (pure in-memory + trait surface).

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::storage_ports::{StorageError, StorageGeneration, StorageResult};
use crate::types::EmbeddingProfileId;

use super::manifest::{BuildStatus, IndexPublicationManifest};
use super::pointer::{ActiveGenerationPointer, PointerEpoch};
use super::validate::validate_for_promotion;

/// Phase of the pure publication state machine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PromotionPhase {
    Staged,
    Validated,
    Promoted,
    RolledBack,
}

/// Typed conflict when concurrent promotion CAS fails.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PromotionConflict {
    pub expected_generation: StorageGeneration,
    pub expected_epoch: PointerEpoch,
    pub actual_generation: StorageGeneration,
    pub actual_epoch: PointerEpoch,
    pub detail: String,
}

impl PromotionConflict {
    pub fn to_storage_error(&self) -> StorageError {
        StorageError::stale_generation(self.expected_generation, self.actual_generation)
    }
}

/// Outcome of a promote or rollback attempt.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum PromotionOutcome {
    Promoted {
        pointer: ActiveGenerationPointer,
        generation: StorageGeneration,
    },
    RolledBack {
        pointer: ActiveGenerationPointer,
        restored_generation: StorageGeneration,
    },
    Conflict(PromotionConflict),
    Rejected {
        reason: String,
    },
}

/// Trait surface for publication coordination (in-memory or durable adapters).
pub trait PublicationCoordinator: Send + Sync {
    /// Stage a manifest without mutating the active pointer.
    fn stage(&mut self, manifest: IndexPublicationManifest) -> StorageResult<()>;

    /// Validate a staged generation for promotion eligibility.
    fn validate(
        &self,
        generation: StorageGeneration,
        expected_profile: Option<&EmbeddingProfileId>,
    ) -> StorageResult<()>;

    /// CAS-promote a Ready generation to active after live re-validation.
    fn promote(
        &mut self,
        generation: StorageGeneration,
        expected_current: StorageGeneration,
        expected_epoch: PointerEpoch,
        updated_at: &str,
        expected_profile: Option<&EmbeddingProfileId>,
    ) -> StorageResult<PromotionOutcome>;

    /// Roll back to the previous generation when recorded on the pointer.
    fn rollback(
        &mut self,
        expected_current: StorageGeneration,
        expected_epoch: PointerEpoch,
        updated_at: &str,
    ) -> StorageResult<PromotionOutcome>;

    fn active_pointer(&self) -> &ActiveGenerationPointer;

    fn get_manifest(&self, generation: StorageGeneration) -> Option<&IndexPublicationManifest>;
}

/// Pure in-memory coordinator for contract tests and single-process staging.
///
/// Promote re-validates live via [`validate_for_promotion`]; there is no
/// separate recorded validate→promote fence in this walking skeleton.
#[derive(Debug)]
pub struct InMemoryPublicationCoordinator {
    pointer: ActiveGenerationPointer,
    manifests: HashMap<u64, IndexPublicationManifest>,
    lock: Mutex<()>,
}

impl InMemoryPublicationCoordinator {
    pub fn new(initial: ActiveGenerationPointer) -> StorageResult<Self> {
        initial.validate()?;
        Ok(Self {
            pointer: initial,
            manifests: HashMap::new(),
            lock: Mutex::new(()),
        })
    }

    fn gen_key(generation: StorageGeneration) -> u64 {
        generation.0
    }
}

impl PublicationCoordinator for InMemoryPublicationCoordinator {
    fn stage(&mut self, manifest: IndexPublicationManifest) -> StorageResult<()> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| StorageError::unavailable("publication coordinator lock poisoned"))?;
        manifest.validate_structure()?;
        // Staging never mutates the active pointer; reject Active payloads and
        // refuse to overwrite the live generation (pointer or registry).
        if matches!(manifest.status, BuildStatus::Active) {
            return Err(StorageError::invalid_request(
                "cannot stage a manifest that is already active; stage as ready/building first",
            ));
        }
        if manifest.generation == self.pointer.active_generation {
            return Err(StorageError::invalid_request(
                "cannot restage the active generation; pointer and registry would desync",
            ));
        }
        let key = Self::gen_key(manifest.generation);
        if self
            .manifests
            .get(&key)
            .is_some_and(|existing| matches!(existing.status, BuildStatus::Active))
        {
            return Err(StorageError::invalid_request(
                "cannot restage a generation already marked Active in the registry",
            ));
        }
        self.manifests.insert(key, manifest);
        Ok(())
    }

    fn validate(
        &self,
        generation: StorageGeneration,
        expected_profile: Option<&EmbeddingProfileId>,
    ) -> StorageResult<()> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| StorageError::unavailable("publication coordinator lock poisoned"))?;
        let manifest = self
            .manifests
            .get(&Self::gen_key(generation))
            .ok_or_else(|| {
                StorageError::not_found("index_publication_manifest", generation.to_string())
            })?;
        validate_for_promotion(manifest, expected_profile).into_result()
    }

    fn promote(
        &mut self,
        generation: StorageGeneration,
        expected_current: StorageGeneration,
        expected_epoch: PointerEpoch,
        updated_at: &str,
        expected_profile: Option<&EmbeddingProfileId>,
    ) -> StorageResult<PromotionOutcome> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| StorageError::unavailable("publication coordinator lock poisoned"))?;

        if self.pointer.active_generation != expected_current
            || self.pointer.epoch != expected_epoch
        {
            return Ok(PromotionOutcome::Conflict(PromotionConflict {
                expected_generation: expected_current,
                expected_epoch,
                actual_generation: self.pointer.active_generation,
                actual_epoch: self.pointer.epoch,
                detail: "concurrent promotion lost CAS on active generation pointer".into(),
            }));
        }

        let key = Self::gen_key(generation);
        let manifest = self.manifests.get(&key).ok_or_else(|| {
            StorageError::not_found("index_publication_manifest", generation.to_string())
        })?;

        let report = validate_for_promotion(manifest, expected_profile);
        if !report.is_ok() {
            let reason = report
                .issues
                .iter()
                .map(|i| format!("{}: {}", i.code, i.detail))
                .collect::<Vec<_>>()
                .join("; ");
            return Ok(PromotionOutcome::Rejected { reason });
        }

        // Construct the next pointer before any registry mutation so empty
        // updated_at (or other pointer validation failure) cannot desync
        // registry statuses from the live pointer.
        let previous = self.pointer.active_generation;
        let mut pointer =
            ActiveGenerationPointer::new(generation, expected_epoch.next(), updated_at)?;
        if previous != generation {
            pointer = pointer.with_previous(previous);
        }

        // Mark previous active (if tracked) as rolled back in our registry.
        if let Some(prev) = self.manifests.get_mut(&Self::gen_key(previous)) {
            if matches!(prev.status, BuildStatus::Active) {
                prev.status = BuildStatus::RolledBack;
            }
        }

        let promoted = self.manifests.get_mut(&key).expect("checked above");
        promoted.status = BuildStatus::Active;
        self.pointer = pointer.clone();

        Ok(PromotionOutcome::Promoted {
            pointer,
            generation,
        })
    }

    fn rollback(
        &mut self,
        expected_current: StorageGeneration,
        expected_epoch: PointerEpoch,
        updated_at: &str,
    ) -> StorageResult<PromotionOutcome> {
        let _guard = self
            .lock
            .lock()
            .map_err(|_| StorageError::unavailable("publication coordinator lock poisoned"))?;

        if self.pointer.active_generation != expected_current
            || self.pointer.epoch != expected_epoch
        {
            return Ok(PromotionOutcome::Conflict(PromotionConflict {
                expected_generation: expected_current,
                expected_epoch,
                actual_generation: self.pointer.active_generation,
                actual_epoch: self.pointer.epoch,
                detail: "concurrent rollback lost CAS on active generation pointer".into(),
            }));
        }

        let Some(previous) = self.pointer.previous_generation else {
            return Ok(PromotionOutcome::Rejected {
                reason: "no previous_generation recorded for rollback".into(),
            });
        };

        // Construct the restored pointer before registry mutation so empty
        // updated_at cannot leave statuses desynced from the live pointer.
        // After rollback, "previous" of the restored gen is the demoted one;
        // further nested history is residual for durable adapters.
        let pointer = ActiveGenerationPointer::new(previous, expected_epoch.next(), updated_at)?
            .with_previous(expected_current);

        // Demote current active.
        let current_key = Self::gen_key(expected_current);
        if let Some(current) = self.manifests.get_mut(&current_key) {
            current.status = BuildStatus::RolledBack;
        }

        // Restore previous if present.
        let prev_key = Self::gen_key(previous);
        if let Some(prev) = self.manifests.get_mut(&prev_key) {
            prev.status = BuildStatus::Active;
        }

        self.pointer = pointer.clone();

        Ok(PromotionOutcome::RolledBack {
            pointer,
            restored_generation: previous,
        })
    }

    fn active_pointer(&self) -> &ActiveGenerationPointer {
        &self.pointer
    }

    fn get_manifest(&self, generation: StorageGeneration) -> Option<&IndexPublicationManifest> {
        self.manifests.get(&Self::gen_key(generation))
    }
}
