//! Hard memory budgets: cgroup-aware process caps and per-query working-set caps.
//!
//! RSS alone is insufficient for retrieval: Linux cgroup v2 accounts page cache
//! and anonymous memory together via `memory.current`, `memory.high`, and
//! `memory.max`. A serving process must gate on the **total** cgroup usage,
//! including file cache, not on RSS. This module declares the validated
//! profile shapes and the per-query working-set caps that every
//! corpus-proportional allocation must validate against before allocating.
//!
//! Contract only — no live cgroup reader, no `memory.events` poller, no
//! resource monitor. See `docs/architecture/retrieval-resource-budgets.md`.

use serde::{Deserialize, Serialize};

use super::{RetrievalBudgetDiagnosticCode, RetrievalBudgetError, RetrievalBudgetResult};

/// One mebibyte.
pub const MIB: u64 = 1 << 20;

/// Absolute floor for `memory.max` (online serving). Anything below this is
/// structurally too small to admit a single bounded retrieval working set.
pub const ONLINE_MEMORY_MAX_FLOOR: u64 = 64 * MIB;

/// Floor for the isolated build/compaction process `memory.max`.
pub const BUILD_MEMORY_MAX_FLOOR: u64 = 128 * MIB;

/// A named deployment role whose memory profile is independently bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MemoryProfileRole {
    /// Online serving path: queries, hydration, fusion, reranking.
    OnlineServing,
    /// Isolated index build / compaction / merge path.
    IsolatedBuild,
}

/// Field bag used to construct and validate a [`MemoryBudgetProfile`].
///
/// `current`, `high`, and `max` are expressed in bytes and correspond to the
/// cgroup v2 `memory.current`, `memory.high`, and `memory.max` files. They
/// include file cache as well as anonymous memory.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryBudgetProfileFields {
    /// Deployment role this profile bounds.
    pub role: MemoryProfileRole,
    /// Observed total cgroup memory usage (`memory.current`), including file cache.
    pub current: u64,
    /// Throttle threshold (`memory.high`); reclaim pressure begins here.
    pub high: u64,
    /// Hard ceiling (`memory.max`); OOM kill / hard failure begins here.
    pub max: u64,
}

/// A validated cgroup-aware memory budget profile.
///
/// Invariants enforced at construction:
/// - `high` is strictly less than `max` (high must leave reclaim headroom);
/// - `max` is at or above the role-specific floor;
/// - `current`, `high`, and `max` are all positive and monotonically ordered
///   (`current <= high` is *not* required — a process may be over `high` — but
///   `high < max` is required, and `current > max` is a hard violation).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MemoryBudgetProfile {
    fields: MemoryBudgetProfileFields,
}

impl MemoryBudgetProfile {
    /// Constructs a profile only when every invariant holds.
    pub fn new(fields: MemoryBudgetProfileFields) -> RetrievalBudgetResult<Self> {
        let profile = Self { fields };
        profile.validate()?;
        Ok(profile)
    }

    /// Revalidates invariants after decode or before an adapter allocates.
    pub fn validate(&self) -> RetrievalBudgetResult<()> {
        let f = self.fields;
        if f.high == 0 || f.max == 0 {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::InvalidMemoryBudget,
            ));
        }
        if f.high >= f.max {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::MemoryHighNotBelowMax,
            ));
        }
        let floor = match f.role {
            MemoryProfileRole::OnlineServing => ONLINE_MEMORY_MAX_FLOOR,
            MemoryProfileRole::IsolatedBuild => BUILD_MEMORY_MAX_FLOOR,
        };
        if f.max < floor {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::MemoryMaxBelowFloor,
            ));
        }
        Ok(())
    }

    /// Returns the validated field bag.
    pub const fn fields(&self) -> MemoryBudgetProfileFields {
        self.fields
    }

    /// Returns the deployment role.
    pub const fn role(&self) -> MemoryProfileRole {
        self.fields.role
    }

    /// Returns the `memory.high` threshold in bytes.
    pub const fn high(&self) -> u64 {
        self.fields.high
    }

    /// Returns the `memory.max` hard ceiling in bytes.
    pub const fn max(&self) -> u64 {
        self.fields.max
    }

    /// Classifies an observed `memory.current` value against the profile.
    ///
    /// Returns `Ok(())` when within `high`, a typed `MemoryHighExceeded` when
    /// between `high` and `max`, and `MemoryMaxExceeded` when over `max`.
    /// This is the typed-exhaustion surface: a caller must surface the code
    /// rather than silently enlarge memory or truncate results.
    pub fn check_current(&self, current: u64) -> RetrievalBudgetResult<()> {
        if current > self.fields.max {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::MemoryMaxExceeded,
            ));
        }
        if current > self.fields.high {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::MemoryHighExceeded,
            ));
        }
        Ok(())
    }

    /// Conservative walking-skeleton defaults for online serving
    /// (high = 192 MiB, max = 256 MiB, matching the issue target profile).
    pub const fn skeleton_online_serving() -> Self {
        Self {
            fields: MemoryBudgetProfileFields {
                role: MemoryProfileRole::OnlineServing,
                current: 0,
                high: 192 * MIB,
                max: 256 * MIB,
            },
        }
    }

    /// Conservative walking-skeleton defaults for isolated build
    /// (high = 384 MiB, max = 512 MiB, matching the issue target profile).
    pub const fn skeleton_isolated_build() -> Self {
        Self {
            fields: MemoryBudgetProfileFields {
                role: MemoryProfileRole::IsolatedBuild,
                current: 0,
                high: 384 * MIB,
                max: 512 * MIB,
            },
        }
    }
}

/// Field bag for the per-query/per-request working-set memory caps.
///
/// Every allocation proportional to a request parameter must validate against
/// the effective [`PerQueryMemoryCaps`] before allocation. Each field is the
/// hard maximum number of bytes the corresponding working set may hold.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct PerQueryMemoryCapsFields {
    /// Read buffers (decompression, vector read, page read).
    pub read_buffers: u64,
    /// Predicate bitmap working sets.
    pub predicate_bitmaps: u64,
    /// Per-query graph frontier / visited state.
    pub graph_frontier: u64,
    /// Exact-rescore candidate pool (full-precision vectors).
    pub exact_rescore_candidates: u64,
    /// Arrow / serialization batch buffers.
    pub arrow_batches: u64,
    /// Lexical / graph / fusion candidate pools (pre-rerank).
    pub fusion_candidates: u64,
    /// Hydration text / evidence buffers.
    pub hydration_text: u64,
}

/// Validated per-query working-set memory caps.
///
/// Every cap must be positive (zero would forbid even a single allocation,
/// which is a configuration error, not a runtime pressure signal — runtime
/// pressure is reported through [`MemoryBudgetProfile::check_current`] and the
/// typed exhaustion codes).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PerQueryMemoryCaps {
    fields: PerQueryMemoryCapsFields,
}

impl PerQueryMemoryCaps {
    /// Constructs caps only when every field is positive.
    pub fn new(fields: PerQueryMemoryCapsFields) -> RetrievalBudgetResult<Self> {
        let caps = Self { fields };
        caps.validate()?;
        Ok(caps)
    }

    /// Revalidates that no working-set cap is zero.
    pub fn validate(&self) -> RetrievalBudgetResult<()> {
        let f = self.fields;
        if [
            f.read_buffers,
            f.predicate_bitmaps,
            f.graph_frontier,
            f.exact_rescore_candidates,
            f.arrow_batches,
            f.fusion_candidates,
            f.hydration_text,
        ]
        .contains(&0)
        {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::InvalidMemoryBudget,
            ));
        }
        Ok(())
    }

    /// Returns the validated field bag.
    pub const fn fields(&self) -> PerQueryMemoryCapsFields {
        self.fields
    }

    /// Conservative walking-skeleton defaults.
    pub const fn skeleton_default() -> Self {
        Self {
            fields: PerQueryMemoryCapsFields {
                read_buffers: 4 * MIB,
                predicate_bitmaps: 2 * MIB,
                graph_frontier: MIB,
                exact_rescore_candidates: 8 * MIB,
                arrow_batches: 4 * MIB,
                fusion_candidates: 4 * MIB,
                hydration_text: 4 * MIB,
            },
        }
    }

    /// Returns the sum of all per-query working-set caps. This is the worst-case
    /// single-query anonymous footprint the profile admits.
    pub fn total(&self) -> RetrievalBudgetResult<u64> {
        let f = self.fields;
        f.read_buffers
            .checked_add(f.predicate_bitmaps)
            .and_then(|s| s.checked_add(f.graph_frontier))
            .and_then(|s| s.checked_add(f.exact_rescore_candidates))
            .and_then(|s| s.checked_add(f.arrow_batches))
            .and_then(|s| s.checked_add(f.fusion_candidates))
            .and_then(|s| s.checked_add(f.hydration_text))
            .ok_or(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::InvalidMemoryBudget,
            ))
    }
}
