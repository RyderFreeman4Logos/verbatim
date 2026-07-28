//! Fixed-maximum capacities for every corpus-proportional cache.
//!
//! Issue #377 requires that **every** cache (page cache, mapping cache, graph
//! cache, DiskANN entry-point cache, etc.) has a fixed configured maximum. No
//! unbounded corpus-proportional structure is permitted. This module declares
//! the validated capacity type and the named cache kinds, so an adapter can
//! prove every cache is bounded before it is constructed.
//!
//! Contract only — no live cache, no eviction policy, no LRU implementation.

use serde::{Deserialize, Serialize};

use super::memory::MIB;
use super::{RetrievalBudgetDiagnosticCode, RetrievalBudgetError, RetrievalBudgetResult};

/// Named corpus-proportional cache kinds that must carry a fixed maximum.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheKind {
    /// Application-owned or hinted SSD page cache.
    PageCache,
    /// Numeric-ID to vector-offset mapping cache.
    MappingCache,
    /// DiskANN graph neighbor / edge cache.
    GraphCache,
    /// DiskANN entry-point / upper-layer cache.
    EntryPointCache,
    /// Decompression working cache.
    DecompressionCache,
    /// Hydrated text / evidence cache.
    HydrationCache,
}

impl CacheKind {
    /// Every named cache kind, useful for exhaustive contract tests.
    pub const ALL: [Self; 6] = [
        Self::PageCache,
        Self::MappingCache,
        Self::GraphCache,
        Self::EntryPointCache,
        Self::DecompressionCache,
        Self::HydrationCache,
    ];

    /// Stable machine-readable name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PageCache => "page_cache",
            Self::MappingCache => "mapping_cache",
            Self::GraphCache => "graph_cache",
            Self::EntryPointCache => "entry_point_cache",
            Self::DecompressionCache => "decompression_cache",
            Self::HydrationCache => "hydration_cache",
        }
    }
}

/// Field bag used to construct and validate a [`CacheCapacity`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CacheCapacityFields {
    /// Which corpus-proportional cache this capacity bounds.
    pub kind: CacheKind,
    /// Fixed maximum size in bytes. Must be positive.
    pub max_bytes: u64,
    /// Size of a single cache entry in bytes (for entry-count validation).
    pub entry_bytes: u64,
}

/// A validated fixed maximum for one corpus-proportional cache.
///
/// `max_bytes` is the absolute ceiling; the cache may hold at most
/// `max_bytes / entry_bytes` entries. Both must be positive, and the byte
/// ceiling must admit at least one full entry — a cache that cannot hold a
/// single entry is a configuration error, not a runtime pressure signal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CacheCapacity {
    fields: CacheCapacityFields,
}

impl CacheCapacity {
    /// Constructs a capacity only when both bounds are positive and the byte
    /// ceiling admits at least one entry.
    pub fn new(fields: CacheCapacityFields) -> RetrievalBudgetResult<Self> {
        let cap = Self { fields };
        cap.validate()?;
        Ok(cap)
    }

    /// Revalidates invariants after decode or before an adapter builds a cache.
    pub fn validate(&self) -> RetrievalBudgetResult<()> {
        let f = self.fields;
        if f.max_bytes == 0 || f.entry_bytes == 0 {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::InvalidCacheCapacity,
            ));
        }
        if f.max_bytes < f.entry_bytes {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::CacheCapacityBelowOneEntry,
            ));
        }
        Ok(())
    }

    /// Returns the validated field bag.
    pub const fn fields(&self) -> CacheCapacityFields {
        self.fields
    }

    /// Returns the cache kind.
    pub const fn kind(&self) -> CacheKind {
        self.fields.kind
    }

    /// Returns the fixed maximum size in bytes.
    pub const fn max_bytes(&self) -> u64 {
        self.fields.max_bytes
    }

    /// Returns the maximum number of entries the cache may hold.
    pub const fn max_entries(&self) -> u64 {
        self.fields.max_bytes / self.fields.entry_bytes
    }

    /// Returns `true` when an observed byte usage stays within the fixed maximum.
    pub const fn admits(&self, used_bytes: u64) -> bool {
        used_bytes <= self.fields.max_bytes
    }

    /// Conservative walking-skeleton default for the SSD page cache.
    pub const fn skeleton_page_cache() -> Self {
        Self {
            fields: CacheCapacityFields {
                kind: CacheKind::PageCache,
                max_bytes: 64 * MIB,
                entry_bytes: 4_096,
            },
        }
    }
}
