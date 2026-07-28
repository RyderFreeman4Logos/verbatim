//! Filter scopes that bound exact scans to contiguous or sorted-ID runs.
//!
//! The scope describes *which* vectors participate in an exact scan. Three
//! structural shapes are supported. Each is validated so that the scan engine
//! never receives an empty, unsorted, or duplicate-bearing ID set.

use serde::{Deserialize, Serialize};

use super::{ExactScanDiagnosticCode, ExactScanError, ExactScanResult};

/// Compact numeric vector identifier (maps to a contiguous storage offset).
pub type VectorOffsetId = u32;

/// A contiguous half-open extent `[start, end)` of vector offsets.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ContiguousExtent {
    pub start: VectorOffsetId,
    pub end: VectorOffsetId,
}

impl ContiguousExtent {
    /// Builds a validated extent. `end` must be strictly greater than `start`.
    pub fn new(start: VectorOffsetId, end: VectorOffsetId) -> ExactScanResult<Self> {
        if end <= start {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::InvalidFilterScope,
            ));
        }
        Ok(Self { start, end })
    }

    /// Returns the number of vector offsets covered.
    pub fn len(&self) -> usize {
        (self.end - self.start) as usize
    }

    /// Returns `false`; a valid extent is never empty.
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Returns `true` when `id` falls within `[start, end)`.
    pub const fn contains(&self, id: VectorOffsetId) -> bool {
        id >= self.start && id < self.end
    }
}

/// A monotonically increasing run of vector IDs with no duplicates.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SortedIdRun {
    ids: Vec<VectorOffsetId>,
}

impl SortedIdRun {
    /// Builds a validated run from caller-owned IDs.
    ///
    /// Rejects empty, unsorted, or duplicate-containing input.
    pub fn new(ids: Vec<VectorOffsetId>) -> ExactScanResult<Self> {
        if ids.is_empty() {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::InvalidFilterScope,
            ));
        }
        for window in ids.windows(2) {
            if window[0] >= window[1] {
                return Err(ExactScanError::contract(
                    ExactScanDiagnosticCode::InvalidFilterScope,
                ));
            }
        }
        Ok(Self { ids })
    }

    /// Returns the number of IDs in the run.
    pub fn len(&self) -> usize {
        self.ids.len()
    }

    /// Returns `false`; a valid run is never empty.
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Returns the underlying ID slice (read-only).
    pub fn as_slice(&self) -> &[VectorOffsetId] {
        &self.ids
    }
}

/// The structural shape of a filter scope used to bound an exact scan.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterScope {
    /// A single contiguous half-open extent `[start, end)`.
    Contiguous(ContiguousExtent),
    /// A monotonically increasing sorted-ID run.
    SortedRun(SortedIdRun),
    /// An arbitrary sparse set of vector IDs (validated for non-emptiness).
    Sparse(Vec<VectorOffsetId>),
}

impl FilterScope {
    /// Returns the count of vector IDs covered by this scope.
    pub fn len(&self) -> usize {
        match self {
            Self::Contiguous(extent) => extent.len(),
            Self::SortedRun(run) => run.len(),
            Self::Sparse(ids) => ids.len(),
        }
    }

    /// Returns `false`; a valid scope is never empty.
    pub const fn is_empty(&self) -> bool {
        false
    }

    /// Builds a sparse scope, rejecting empty input.
    pub fn sparse(ids: Vec<VectorOffsetId>) -> ExactScanResult<Self> {
        if ids.is_empty() {
            return Err(ExactScanError::contract(
                ExactScanDiagnosticCode::InvalidFilterScope,
            ));
        }
        Ok(Self::Sparse(ids))
    }
}

#[cfg(test)]
mod scope_tests {
    use super::*;

    #[test]
    fn contiguous_extent_valid() {
        let ext = ContiguousExtent::new(0, 100).unwrap();
        assert_eq!(ext.len(), 100);
        assert!(ext.contains(50));
        assert!(!ext.contains(100));
        assert!(!ext.is_empty());
    }

    #[test]
    fn contiguous_extent_rejects_empty_and_inverted() {
        assert!(ContiguousExtent::new(10, 10).is_err());
        assert!(ContiguousExtent::new(20, 10).is_err());
    }

    #[test]
    fn sorted_run_valid() {
        let run = SortedIdRun::new(vec![3, 7, 11, 42]).unwrap();
        assert_eq!(run.len(), 4);
        assert_eq!(run.as_slice(), &[3, 7, 11, 42]);
    }

    #[test]
    fn sorted_run_rejects_empty() {
        assert!(SortedIdRun::new(vec![]).is_err());
    }

    #[test]
    fn sorted_run_rejects_unsorted_and_duplicates() {
        assert!(SortedIdRun::new(vec![1, 3, 2]).is_err());
        assert!(SortedIdRun::new(vec![1, 1, 2]).is_err());
    }

    #[test]
    fn sparse_scope_valid() {
        let scope = FilterScope::sparse(vec![1, 5, 9]).unwrap();
        assert_eq!(scope.len(), 3);
    }

    #[test]
    fn sparse_scope_rejects_empty() {
        assert!(FilterScope::sparse(vec![]).is_err());
    }
}
