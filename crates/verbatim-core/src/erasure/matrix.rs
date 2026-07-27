//! Complete propagation matrix and fail-closed ordering validation.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use super::{
    DataProduct, DeletionState, DeletionTarget, ErasureDiagnosticCode, ErasureError, ErasureResult,
};

/// Globally enforced ordering: authoritative records, derived products, caches,
/// then remote replicas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletionOrdering {
    Authoritative,
    Derived,
    Cache,
    RemoteReplica,
}

/// Required treatment for one deletion target.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionMatrixEntry {
    pub target: DeletionTarget,
    pub product: DataProduct,
    pub state: DeletionState,
    pub ordering: DeletionOrdering,
}

/// Full inventory mapping. Replacing an entry requires preserving its exact
/// product/state/order classification, preventing a backend from being omitted
/// or silently downgraded during an erasure plan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionPropagationMatrix {
    entries: Vec<DeletionMatrixEntry>,
}

impl DeletionPropagationMatrix {
    pub fn new(entries: Vec<DeletionMatrixEntry>) -> Self {
        Self { entries }
    }

    pub fn canonical() -> Self {
        Self::new(vec![
            DeletionMatrixEntry {
                target: DeletionTarget::Sqlite,
                product: DataProduct::Authoritative,
                state: DeletionState::Tombstone,
                ordering: DeletionOrdering::Authoritative,
            },
            DeletionMatrixEntry {
                target: DeletionTarget::Tantivy,
                product: DataProduct::DerivedRebuildable,
                state: DeletionState::ImmediatePhysicalErase,
                ordering: DeletionOrdering::Derived,
            },
            DeletionMatrixEntry {
                target: DeletionTarget::Hnsw,
                product: DataProduct::DerivedRebuildable,
                state: DeletionState::ImmediatePhysicalErase,
                ordering: DeletionOrdering::Derived,
            },
            DeletionMatrixEntry {
                target: DeletionTarget::GraphNodes,
                product: DataProduct::DerivedRebuildable,
                state: DeletionState::ImmediatePhysicalErase,
                ordering: DeletionOrdering::Derived,
            },
            DeletionMatrixEntry {
                target: DeletionTarget::GraphEdges,
                product: DataProduct::DerivedRebuildable,
                state: DeletionState::ImmediatePhysicalErase,
                ordering: DeletionOrdering::Derived,
            },
            DeletionMatrixEntry {
                target: DeletionTarget::GraphReports,
                product: DataProduct::RetainedForAudit,
                state: DeletionState::DelayedBackupExpiry,
                ordering: DeletionOrdering::Derived,
            },
            DeletionMatrixEntry {
                target: DeletionTarget::Blobs,
                product: DataProduct::MustDelete,
                state: DeletionState::ImmediatePhysicalErase,
                ordering: DeletionOrdering::Derived,
            },
            DeletionMatrixEntry {
                target: DeletionTarget::Images,
                product: DataProduct::MustDelete,
                state: DeletionState::ImmediatePhysicalErase,
                ordering: DeletionOrdering::Derived,
            },
            DeletionMatrixEntry {
                target: DeletionTarget::Tasks,
                product: DataProduct::DerivedRebuildable,
                state: DeletionState::Quarantine,
                ordering: DeletionOrdering::Derived,
            },
            DeletionMatrixEntry {
                target: DeletionTarget::WorkflowArtifacts,
                product: DataProduct::DerivedRebuildable,
                state: DeletionState::Quarantine,
                ordering: DeletionOrdering::Derived,
            },
            DeletionMatrixEntry {
                target: DeletionTarget::Exports,
                product: DataProduct::MustDelete,
                state: DeletionState::ImmediatePhysicalErase,
                ordering: DeletionOrdering::Derived,
            },
            DeletionMatrixEntry {
                target: DeletionTarget::TemporaryUploads,
                product: DataProduct::MustDelete,
                state: DeletionState::ImmediatePhysicalErase,
                ordering: DeletionOrdering::Derived,
            },
            DeletionMatrixEntry {
                target: DeletionTarget::QueryCache,
                product: DataProduct::Tombstoned,
                state: DeletionState::LogicalDelete,
                ordering: DeletionOrdering::Cache,
            },
            DeletionMatrixEntry {
                target: DeletionTarget::ContextCache,
                product: DataProduct::Tombstoned,
                state: DeletionState::LogicalDelete,
                ordering: DeletionOrdering::Cache,
            },
            DeletionMatrixEntry {
                target: DeletionTarget::AnswerCache,
                product: DataProduct::Tombstoned,
                state: DeletionState::LogicalDelete,
                ordering: DeletionOrdering::Cache,
            },
            DeletionMatrixEntry {
                target: DeletionTarget::Qdrant,
                product: DataProduct::DerivedRebuildable,
                state: DeletionState::ImmediatePhysicalErase,
                ordering: DeletionOrdering::RemoteReplica,
            },
        ])
    }

    pub fn entries(&self) -> &[DeletionMatrixEntry] {
        &self.entries
    }

    pub fn entry(&self, target: DeletionTarget) -> Option<DeletionMatrixEntry> {
        self.entries
            .iter()
            .copied()
            .find(|entry| entry.target == target)
    }

    pub fn ordered_targets(&self) -> Vec<DeletionTarget> {
        self.entries.iter().map(|entry| entry.target).collect()
    }

    pub fn validate(&self) -> ErasureResult<()> {
        let canonical = Self::canonical();
        if self.entries.len() != DeletionTarget::ALL.len() {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::MatrixCoverageMissing,
            ));
        }
        let targets: BTreeSet<_> = self.entries.iter().map(|entry| entry.target).collect();
        if targets.len() != DeletionTarget::ALL.len()
            || !DeletionTarget::ALL
                .iter()
                .all(|target| targets.contains(target))
        {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::MatrixCoverageMissing,
            ));
        }
        for (position, entry) in self.entries.iter().enumerate() {
            let expected = canonical.entries[position];
            if entry.target != expected.target
                || entry.product != expected.product
                || entry.state != expected.state
            {
                return Err(ErasureError::validation(
                    ErasureDiagnosticCode::MatrixClassificationMismatch,
                ));
            }
            if entry.ordering != expected.ordering {
                return Err(ErasureError::validation(
                    ErasureDiagnosticCode::MatrixOrderingInvalid,
                ));
            }
        }
        Ok(())
    }
}
