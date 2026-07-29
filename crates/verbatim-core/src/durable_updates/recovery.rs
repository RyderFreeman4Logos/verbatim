//! Crash-recovery contract: previous committed, new committed, or rejected.
//!
//! Recovery may yield the previous committed state (if the new state was not
//! fully validated and published) or the complete new committed state (if
//! publication was durable). It may **never** yield a search-visible mixture
//! whose manifest claims success but whose data is partially written. A
//! [`CrashRecoveryResult::InconsistentRejected`] quarantines the shard and
//! forces rebuild from authoritative vectors.

use serde::{Deserialize, Serialize};

use super::identity::DurableGeneration;
use super::mutation::MutationStage;
use super::{DurableUpdateDiagnosticCode, DurableUpdateError, DurableUpdateResult};

/// Durable fsync attestation for a recovery checkpoint, mirroring the shard
/// build contract. A recovery that claims `Published` requires both data and
/// directory fsync.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryFsyncAttestation {
    data_fsynced: bool,
    dir_fsynced: bool,
}

impl RecoveryFsyncAttestation {
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

/// A crash-recovery observation: what the recovery procedure found on disk.
///
/// The procedure inspects the operation log, the last checkpoint, and the
/// publication manifest. It then produces one of three outcomes. Mixed or
/// unvalidated state is never published.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CrashRecoveryResult {
    /// The new state was not durably published; the previous committed
    /// generation remains the live, search-visible state.
    PreviousCommitted { generation: DurableGeneration },
    /// The new state was fully validated, fsynced, and durably published; it is
    /// now the live, search-visible generation.
    NewCommitted {
        generation: DurableGeneration,
        stage: MutationStage,
        fsync: RecoveryFsyncAttestation,
    },
    /// The on-disk state is inconsistent (partially written, unvalidated, or
    /// its manifest claims success without fsync attestation). The shard is
    /// quarantined and must be rebuilt from authoritative vectors.
    InconsistentRejected { code: DurableUpdateDiagnosticCode },
}

impl CrashRecoveryResult {
    /// Decides the recovery outcome from the observed last stage, generation,
    /// and fsync attestation.
    ///
    /// - If the stage is `Published` and fsync is fully durable, the new state
    ///   is committed.
    /// - If the stage is durable but not published (`Checkpointed`, `Compacted`,
    ///   `Validated`), the previous committed generation survives and the new
    ///   state is discarded.
    /// - If the stage is `Published` but fsync is **not** fully durable, the
    ///   manifest lies about durability and the state is rejected.
    /// - Any pre-checkpoint stage yields the previous committed generation.
    pub fn decide(
        observed_generation: DurableGeneration,
        last_stage: MutationStage,
        fsync: RecoveryFsyncAttestation,
        previous_generation: DurableGeneration,
    ) -> Self {
        match last_stage {
            MutationStage::Published => {
                if fsync.is_fully_durable() {
                    Self::NewCommitted {
                        generation: observed_generation,
                        stage: last_stage,
                        fsync,
                    }
                } else {
                    Self::InconsistentRejected {
                        code: DurableUpdateDiagnosticCode::InconsistentRecovery,
                    }
                }
            }
            stage if stage.is_durable() => Self::PreviousCommitted {
                generation: previous_generation,
            },
            _ => Self::PreviousCommitted {
                generation: previous_generation,
            },
        }
    }

    /// Returns `true` if recovery yielded a consistent, search-visible state
    /// (either previous or new committed).
    pub const fn is_consistent(&self) -> bool {
        matches!(
            self,
            Self::PreviousCommitted { .. } | Self::NewCommitted { .. }
        )
    }

    /// Returns `true` only if recovery rejected the on-disk state as inconsistent.
    pub const fn is_rejected(&self) -> bool {
        matches!(self, Self::InconsistentRejected { .. })
    }

    /// Returns the diagnostic code carried by an `InconsistentRejected`
    /// result, or `None` for consistent outcomes.
    pub const fn rejection_code(&self) -> Option<DurableUpdateDiagnosticCode> {
        match self {
            Self::InconsistentRejected { code } => Some(*code),
            _ => None,
        }
    }
}

/// Validates that a source replacement does not expose old and new chunks
/// together under one active generation. The replacement is atomic: the old
/// vector ids must be retired (tombstoned) in the same generation that
/// publishes the new ids.
pub fn validate_source_replace_atomicity(
    generation: DurableGeneration,
    old_ids_published: bool,
    new_ids_published: bool,
) -> DurableUpdateResult<()> {
    if old_ids_published && new_ids_published {
        return Err(DurableUpdateError::contract(
            DurableUpdateDiagnosticCode::SourceReplaceVisibilityViolation,
        ));
    }
    // The generation itself is validated by construction; this guard exists to
    // make the atomicity rule explicit and testable.
    let _ = generation;
    Ok(())
}
