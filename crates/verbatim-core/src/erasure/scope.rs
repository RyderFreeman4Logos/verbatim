//! Request scope and stale-read fence for one deletion lifecycle.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::{
    DataProduct, DeletionPropagationMatrix, DeletionState, DeletionTarget, ErasureDiagnosticCode,
    ErasureError, ErasureResult,
};

/// A source-bounded erasure request. Source identifiers are inputs to planning
/// only; reports deliberately retain only a non-reversible commitment.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeletionScope {
    pub source_ids: Vec<String>,
    pub targets: BTreeSet<DeletionTarget>,
    pub products: BTreeMap<DeletionTarget, DataProduct>,
    pub state: DeletionState,
}

impl DeletionScope {
    /// Create a complete, initially logically-deleted scope from a validated
    /// matrix. Logical deletion immediately fences serving before cleanup.
    pub fn new(source_ids: Vec<String>, matrix: &DeletionPropagationMatrix) -> ErasureResult<Self> {
        let scope = Self {
            source_ids,
            targets: matrix.entries().iter().map(|entry| entry.target).collect(),
            products: matrix
                .entries()
                .iter()
                .map(|entry| (entry.target, entry.product))
                .collect(),
            state: DeletionState::LogicalDelete,
        };
        scope.validate(matrix)?;
        Ok(scope)
    }

    /// A planned deletion scope always denies stale reads before asynchronous
    /// propagation, including while a remote replica awaits reconciliation.
    pub const fn blocks_serving(&self) -> bool {
        true
    }

    pub fn validate(&self, matrix: &DeletionPropagationMatrix) -> ErasureResult<()> {
        matrix.validate()?;
        if self.source_ids.is_empty() {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::ScopeSourceIdsRequired,
            ));
        }
        let mut unique_source_ids = BTreeSet::new();
        for source_id in &self.source_ids {
            if source_id.trim().is_empty() {
                return Err(ErasureError::validation(
                    ErasureDiagnosticCode::ScopeSourceIdInvalid,
                ));
            }
            if !unique_source_ids.insert(source_id) {
                return Err(ErasureError::validation(
                    ErasureDiagnosticCode::ScopeSourceIdsDuplicate,
                ));
            }
        }
        let expected_targets: BTreeSet<_> =
            matrix.entries().iter().map(|entry| entry.target).collect();
        if self.targets != expected_targets || self.products.len() != expected_targets.len() {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::ScopeTargetSetIncomplete,
            ));
        }
        for entry in matrix.entries() {
            if self.products.get(&entry.target) != Some(&entry.product) {
                return Err(ErasureError::validation(
                    ErasureDiagnosticCode::ScopeProductMismatch,
                ));
            }
        }
        if self.state != DeletionState::LogicalDelete {
            return Err(ErasureError::validation(
                ErasureDiagnosticCode::ScopeInitialStateInvalid,
            ));
        }
        Ok(())
    }
}
