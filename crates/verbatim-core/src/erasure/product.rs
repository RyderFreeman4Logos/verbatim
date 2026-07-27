//! Data-product classifications used to select deletion treatment.

use serde::{Deserialize, Serialize};

/// Classification of a data product, independent of its current backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DataProduct {
    Authoritative,
    DerivedRebuildable,
    RetainedForAudit,
    Tombstoned,
    MustDelete,
}

impl DataProduct {
    pub const ALL: [Self; 5] = [
        Self::Authoritative,
        Self::DerivedRebuildable,
        Self::RetainedForAudit,
        Self::Tombstoned,
        Self::MustDelete,
    ];
}
