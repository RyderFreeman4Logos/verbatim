//! Hard memory / page-cache / SSD-I/O / concurrency budgets for retrieval.
//!
//! This pure contract module turns Verbatim's desired low-memory behavior into
//! enforceable online limits. It declares the validated budget types for:
//!
//! - **Memory**: cgroup-aware process profiles (`memory.current` including file
//!   cache, `memory.high`, `memory.max`) and per-query working-set caps.
//! - **Corpus-proportional caches**: fixed configured maximums for every cache
//!   (page cache, mapping cache, graph cache, etc.). No unbounded structure.
//! - **I/O**: pages, bytes, IOPS, queue depth, await time, read amplification,
//!   direct/buffered/mmap mode. Typed partial/failure states on exhaustion.
//! - **Concurrency**: bounded concurrent model, storage, and retrieval work;
//!   embedding/reranking cannot evict the entire search working set.
//! - **Process isolation**: separate build/compaction from online serving with
//!   independent cgroups and CPU/I/O priorities.
//! - **Typed exhaustion**: budget exhaustion is a typed enum, never an
//!   unmarked empty/partial result.
//!
//! It is deliberately a **walking skeleton**: no live cgroup reader, no
//! resource monitor, no DiskANN3 binding, no runtime spawn. Future adapters
//! must implement these boundaries before they can participate in retrieval.
//!
//! See `docs/architecture/retrieval-resource-budgets.md`. Refs #377.

mod account;
mod cache;
mod concurrency;
mod error;
mod io;
mod isolation;
mod memory;

pub use account::ResourceAccount;
pub use cache::{CacheCapacity, CacheCapacityFields, CacheKind};
pub use concurrency::{ConcurrencyBudget, ConcurrencyBudgetFields, WorkCategory};
pub use error::{RetrievalBudgetDiagnosticCode, RetrievalBudgetError, RetrievalBudgetResult};
pub use io::{IoAccessMode, IoBudget, IoBudgetFields, ResourceExhaustion, READ_AMP_DENOMINATOR};
pub use isolation::{
    CpuPriorityClass, IoPriorityClass, ProcessIsolationSpec, ProcessIsolationSpecFields,
};
pub use memory::{
    MemoryBudgetProfile, MemoryBudgetProfileFields, MemoryProfileRole, PerQueryMemoryCaps,
    PerQueryMemoryCapsFields, BUILD_MEMORY_MAX_FLOOR, ONLINE_MEMORY_MAX_FLOOR,
};

/// Contract schema version for retrieval-resource-budget documents.
pub const RETRIEVAL_BUDGETS_CONTRACT_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
