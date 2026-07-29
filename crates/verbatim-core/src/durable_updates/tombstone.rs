//! Generation- and version-aware tombstones with capped delta memory.
//!
//! A [`TombstoneSet`] excludes tombstoned vector ids from search results before
//! hydration. Tombstones are generation- and version-aware: a tombstone recorded
//! in generation G is invisible to searches bound to generation G-1 (which may
//! still be served under a live query lease), and a tombstone for version V does
//! not suppress a vector that was re-inserted at version V+1. The set's memory is
//! capped by a documented maximum tombstone count.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::identity::{DurableGeneration, DurableVectorId, MutationVersion};
use super::{DurableUpdateDiagnosticCode, DurableUpdateError, DurableUpdateResult};

/// One generation- and version-aware soft-deletion tombstone.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct Tombstone {
    vector_id: DurableVectorId,
    generation: DurableGeneration,
    version: MutationVersion,
}

impl Tombstone {
    /// Constructs a tombstone bound to the generation and version that produced it.
    pub const fn new(
        vector_id: DurableVectorId,
        generation: DurableGeneration,
        version: MutationVersion,
    ) -> Self {
        Self {
            vector_id,
            generation,
            version,
        }
    }

    /// Returns the tombstoned stable vector identity.
    pub const fn vector_id(&self) -> DurableVectorId {
        self.vector_id
    }

    /// Returns the generation in which the tombstone was recorded.
    pub const fn generation(&self) -> DurableGeneration {
        self.generation
    }

    /// Returns the version at which the tombstone was recorded.
    pub const fn version(&self) -> MutationVersion {
        self.version
    }
}

/// Capped, generation-aware set of soft-deletion tombstones.
///
/// Search excludes tombstoned ids **before hydration**: a candidate whose
/// `vector_id` is tombstoned in the search's generation at a version `>=` the
/// candidate's index version is removed without fetching its payload. The set's
/// cardinality is capped; exceeding the cap triggers compaction rather than
/// unbounded growth.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstoneSet {
    /// vector_id → latest tombstone (generation, version) for that id.
    tombstones: BTreeMap<DurableVectorId, Tombstone>,
    cap: usize,
}

impl TombstoneSet {
    /// Default upper bound on tombstone/delta entries before compaction is forced.
    pub const DEFAULT_CAP: usize = 100_000;

    /// Constructs an empty tombstone set with the default cap.
    pub fn new() -> Self {
        Self::with_cap(Self::DEFAULT_CAP)
    }

    /// Constructs an empty tombstone set with a caller-specified cap.
    pub fn with_cap(cap: usize) -> Self {
        Self {
            tombstones: BTreeMap::new(),
            cap,
        }
    }

    /// Records a tombstone, replacing any prior tombstone for the same vector id
    /// if the new one is at a strictly greater version. Re-recording the same
    /// (generation, version) is idempotent. A tombstone whose version is older
    /// than the recorded version is rejected.
    pub fn record(&mut self, tombstone: Tombstone) -> DurableUpdateResult<()> {
        match self.tombstones.get(&tombstone.vector_id) {
            Some(existing) if existing.version.value() > tombstone.version.value() => {
                return Err(DurableUpdateError::contract(
                    DurableUpdateDiagnosticCode::VersionOutOfOrder,
                ));
            }
            _ => {}
        }
        if !self.tombstones.contains_key(&tombstone.vector_id) && self.tombstones.len() >= self.cap
        {
            return Err(DurableUpdateError::contract(
                DurableUpdateDiagnosticCode::TombstoneCapExceeded,
            ));
        }
        self.tombstones.insert(tombstone.vector_id, tombstone);
        Ok(())
    }

    /// Returns `true` if `vector_id` is tombstoned in `generation` at a version
    /// greater than or equal to `index_version`. This is the pre-hydration
    /// exclusion predicate.
    pub fn is_excluded(
        &self,
        vector_id: DurableVectorId,
        generation: DurableGeneration,
        index_version: MutationVersion,
    ) -> bool {
        match self.tombstones.get(&vector_id) {
            Some(tombstone) => {
                tombstone.generation.value() <= generation.value()
                    && tombstone.version.value() >= index_version.value()
            }
            None => false,
        }
    }

    /// Filters a slice of `(vector_id, index_version)` pairs, removing those that
    /// are tombstoned in `generation`. Returns the survivors in input order.
    pub fn exclude_tombstoned<'a>(
        &self,
        candidates: &'a [(DurableVectorId, MutationVersion)],
        generation: DurableGeneration,
    ) -> Vec<&'a (DurableVectorId, MutationVersion)> {
        candidates
            .iter()
            .filter(|(vector_id, index_version)| {
                !self.is_excluded(*vector_id, generation, *index_version)
            })
            .collect()
    }

    /// Returns the number of tombstoned vector ids.
    pub fn len(&self) -> usize {
        self.tombstones.len()
    }

    /// Returns `true` if no tombstones are recorded.
    pub fn is_empty(&self) -> bool {
        self.tombstones.is_empty()
    }

    /// Returns the configured maximum tombstone count.
    pub const fn cap(&self) -> usize {
        self.cap
    }

    /// Returns `true` if the set has reached its cap (compaction should trigger).
    pub fn is_at_cap(&self) -> bool {
        self.tombstones.len() >= self.cap
    }
}

impl Default for TombstoneSet {
    fn default() -> Self {
        Self::new()
    }
}
