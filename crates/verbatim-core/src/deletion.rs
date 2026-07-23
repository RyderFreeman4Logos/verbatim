use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};
#[cfg(test)]
use std::collections::{HashMap, HashSet};

use crate::types::SourceId;

/// Storage products that participate in a source-erasure request.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize)]
pub enum DeletionProduct {
    SqliteAuthoritative,
    Chunks,
    Vectors,
    Hnsw,
    Qdrant,
    Graph,
    Images,
    Caches,
    Backups,
}

impl DeletionProduct {
    pub const ALL: [Self; 9] = [
        Self::SqliteAuthoritative,
        Self::Chunks,
        Self::Vectors,
        Self::Hnsw,
        Self::Qdrant,
        Self::Graph,
        Self::Images,
        Self::Caches,
        Self::Backups,
    ];
}

/// Terminal or retryable state of one deletion product.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum DeletionOutcome {
    Erased,
    Pending,
    Held,
    NotFound,
}

/// A content-free receipt for one cross-backend deletion request.
#[derive(Clone, Deserialize, PartialEq, Eq, Serialize)]
pub struct DeletionReport {
    outcomes: BTreeMap<DeletionProduct, DeletionOutcome>,
}

impl DeletionReport {
    pub fn new() -> Self {
        let outcomes = DeletionProduct::ALL
            .into_iter()
            .map(|product| (product, DeletionOutcome::NotFound))
            .collect();
        Self { outcomes }
    }

    /// Return the outcome recorded for one storage product.
    pub fn status_for(&self, product: DeletionProduct) -> Option<DeletionOutcome> {
        self.outcomes.get(&product).copied()
    }

    pub(crate) fn set(&mut self, product: DeletionProduct, outcome: DeletionOutcome) {
        self.outcomes.insert(product, outcome);
    }
}

impl Default for DeletionReport {
    fn default() -> Self {
        Self::new()
    }
}

impl fmt::Debug for DeletionReport {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("DeletionReport")
            .field("outcomes", &self.outcomes)
            .finish()
    }
}

/// Durable, content-free record of one deletion attempt or reconciliation retry.
#[derive(Clone, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub struct PersistedDeletionReport {
    pub source_id: SourceId,
    pub recorded_at: String,
    pub retention_policy: RetentionPolicy,
    pub report: DeletionReport,
}

/// Backup-retention state associated with a tombstoned source.
#[derive(Clone, Copy, Debug, Deserialize, PartialEq, Eq, Serialize)]
pub enum RetentionPolicy {
    Immediate,
    UntilBackupExpiry(u64),
    LegalHold,
}

impl RetentionPolicy {
    pub const fn until_backup_expiry(timestamp: u64) -> Self {
        Self::UntilBackupExpiry(timestamp)
    }

    pub const fn backup_outcome_at(self, now: u64) -> DeletionOutcome {
        match self {
            Self::Immediate => DeletionOutcome::Erased,
            Self::UntilBackupExpiry(expiry) if now >= expiry => DeletionOutcome::Erased,
            Self::UntilBackupExpiry(_) => DeletionOutcome::Pending,
            Self::LegalHold => DeletionOutcome::Held,
        }
    }
}

#[cfg(test)]
struct TrackedDeletion {
    retention_policy: RetentionPolicy,
    remote_outcome: DeletionOutcome,
}

/// In-memory lifecycle model used to unit-test erasure state transitions.
#[cfg(test)]
#[derive(Default)]
pub(crate) struct DeletionLifecycle {
    registered_sources: HashSet<String>,
    tombstones: HashMap<String, TrackedDeletion>,
    unavailable_backends: HashSet<DeletionProduct>,
    legal_holds: HashSet<String>,
}

#[cfg(test)]
impl DeletionLifecycle {
    pub(crate) fn new() -> Self {
        Self::default()
    }

    pub(crate) fn register_source(&mut self, source_id: &str) {
        self.registered_sources.insert(source_id.into());
    }

    pub(crate) fn set_backend_unavailable(&mut self, product: DeletionProduct, _reason: &str) {
        self.unavailable_backends.insert(product);
    }

    pub(crate) fn set_backend_available(&mut self, product: DeletionProduct) {
        self.unavailable_backends.remove(&product);
    }

    pub(crate) fn delete(
        &mut self,
        source_id: &str,
        retention_policy: RetentionPolicy,
    ) -> DeletionReport {
        let mut report = DeletionReport::new();
        if !self.registered_sources.remove(source_id) {
            return report;
        }
        for product in [
            DeletionProduct::SqliteAuthoritative,
            DeletionProduct::Chunks,
            DeletionProduct::Vectors,
            DeletionProduct::Graph,
        ] {
            report.set(product, DeletionOutcome::Erased);
        }
        report.set(DeletionProduct::Hnsw, DeletionOutcome::Pending);
        report.set(DeletionProduct::Images, DeletionOutcome::Pending);
        report.set(DeletionProduct::Caches, DeletionOutcome::Pending);
        let remote_outcome = self.remote_outcome();
        report.set(DeletionProduct::Qdrant, remote_outcome);
        report.set(
            DeletionProduct::Backups,
            retention_policy.backup_outcome_at(0),
        );
        self.tombstones.insert(
            source_id.into(),
            TrackedDeletion {
                retention_policy,
                remote_outcome,
            },
        );
        report
    }

    pub(crate) fn reconcile(
        &mut self,
        source_id: &str,
        now: u64,
    ) -> Result<DeletionReport, &'static str> {
        let remote_outcome = self.remote_outcome();
        let tracked = self
            .tombstones
            .get_mut(source_id)
            .ok_or("source is not tombstoned")?;
        if tracked.remote_outcome == DeletionOutcome::Pending {
            tracked.remote_outcome = remote_outcome;
        }
        let mut report = DeletionReport::new();
        report.set(DeletionProduct::Qdrant, tracked.remote_outcome);
        let backups = if self.legal_holds.contains(source_id) {
            DeletionOutcome::Held
        } else {
            tracked.retention_policy.backup_outcome_at(now)
        };
        report.set(DeletionProduct::Backups, backups);
        Ok(report)
    }

    pub(crate) fn place_legal_hold(&mut self, source_id: &str) -> Result<(), &'static str> {
        if !self.tombstones.contains_key(source_id) {
            return Err("source is not tombstoned");
        }
        self.legal_holds.insert(source_id.into());
        Ok(())
    }

    pub(crate) fn release_legal_hold(&mut self, source_id: &str) -> Result<(), &'static str> {
        if !self.tombstones.contains_key(source_id) {
            return Err("source is not tombstoned");
        }
        self.legal_holds.remove(source_id);
        Ok(())
    }

    pub(crate) fn can_serve(&self, source_id: &str) -> bool {
        self.registered_sources.contains(source_id) && !self.tombstones.contains_key(source_id)
    }

    pub(crate) fn can_restore(&self, source_id: &str) -> bool {
        !self.tombstones.contains_key(source_id)
    }

    fn remote_outcome(&self) -> DeletionOutcome {
        if self.unavailable_backends.contains(&DeletionProduct::Qdrant) {
            DeletionOutcome::Pending
        } else {
            DeletionOutcome::Erased
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{DeletionLifecycle, DeletionOutcome, DeletionProduct, RetentionPolicy};

    #[test]
    fn deletion_blocks_service_then_reconciles_remote_and_expires_backup_without_leaking_content() {
        let mut lifecycle = DeletionLifecycle::new();
        let source_id = "src-363";
        let restricted_content = "restricted deletion payload must never appear in an audit report";
        lifecycle.register_source(source_id);
        lifecycle.set_backend_unavailable(DeletionProduct::Qdrant, restricted_content);

        let report = lifecycle.delete(source_id, RetentionPolicy::until_backup_expiry(20));

        assert!(!lifecycle.can_serve(source_id));
        assert_eq!(
            report.status_for(DeletionProduct::Qdrant),
            Some(DeletionOutcome::Pending),
        );
        assert_eq!(
            report.status_for(DeletionProduct::Backups),
            Some(DeletionOutcome::Pending),
        );
        assert!(!format!("{report:?}").contains(restricted_content));

        lifecycle.set_backend_available(DeletionProduct::Qdrant);
        let reconciled = lifecycle.reconcile(source_id, 10).unwrap();
        assert_eq!(
            reconciled.status_for(DeletionProduct::Qdrant),
            Some(DeletionOutcome::Erased),
        );
        assert_eq!(
            reconciled.status_for(DeletionProduct::Backups),
            Some(DeletionOutcome::Pending),
        );

        lifecycle.place_legal_hold(source_id).unwrap();
        let held = lifecycle.reconcile(source_id, 20).unwrap();
        assert_eq!(
            held.status_for(DeletionProduct::Backups),
            Some(DeletionOutcome::Held),
        );
        lifecycle.release_legal_hold(source_id).unwrap();
        let erased = lifecycle.reconcile(source_id, 20).unwrap();
        assert_eq!(
            erased.status_for(DeletionProduct::Backups),
            Some(DeletionOutcome::Erased),
        );
        assert!(!lifecycle.can_restore(source_id));
    }
}
