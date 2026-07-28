//! Hard SSD I/O budgets: pages, bytes, IOPS, queue depth, await time, read
//! amplification, and direct/buffered/mmap mode.
//!
//! A retrieval request may not traverse indefinitely after its deadline or
//! page cap. This module declares the validated I/O budget and the typed
//! exhaustion state surfaced when any dimension is exceeded. It extends the
//! page/byte caps already present in `SearchBudget` with the storage-layer
//! dimensions issue #377 names explicitly: IOPS, queue depth, await time,
//! read amplification, access mode, cache hits/misses, and major faults.
//!
//! Contract only — no live I/O, no `iostat`, no `/proc/diskstats` reader.

use serde::{Deserialize, Serialize};

use super::{RetrievalBudgetDiagnosticCode, RetrievalBudgetError, RetrievalBudgetResult};

/// SSD read access mode, controlling whether the kernel page cache is used.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum IoAccessMode {
    /// `O_DIRECT`: bypass kernel page cache; DMA into user buffers.
    Direct,
    /// Buffered I/O through the kernel page cache.
    Buffered,
    /// Memory-mapped read (`mmap`).
    Mmap,
}

impl IoAccessMode {
    /// Every access mode, useful for exhaustive contract tests.
    pub const ALL: [Self; 3] = [Self::Direct, Self::Buffered, Self::Mmap];

    /// Stable machine-readable name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Buffered => "buffered",
            Self::Mmap => "mmap",
        }
    }
}

/// Field bag used to construct and validate an [`IoBudget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IoBudgetFields {
    /// Maximum SSD pages read per request.
    pub max_pages: u64,
    /// Maximum bytes read per request.
    pub max_bytes: u64,
    /// Maximum read operations (IOPS) per request.
    pub max_iops: u64,
    /// Maximum outstanding read operations (queue depth).
    pub max_queue_depth: u16,
    /// Maximum cumulative await time in microseconds.
    pub max_await_micros: u64,
    /// Maximum read-amplification ratio (pages read per vector rescored),
    /// expressed as a fixed-point numerator over `READ_AMP_DENOMINATOR`.
    pub max_read_amplification: u32,
    /// Access mode governing kernel page-cache use.
    pub access_mode: IoAccessMode,
}

/// Denominator for the fixed-point read-amplification ceiling.
/// A ratio of `n / READ_AMP_DENOMINATOR` means at most `n` pages read per
/// vector rescored. `1000` gives three decimals of precision.
pub const READ_AMP_DENOMINATOR: u32 = 1_000;

/// A validated hard I/O budget for one retrieval request.
///
/// Every field must be positive. Read amplification is bounded separately from
/// raw page counts so a pathological expansion cannot hide behind a generous
/// page cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IoBudget {
    fields: IoBudgetFields,
}

impl IoBudget {
    /// Constructs a budget only when every field is positively bounded.
    pub fn new(fields: IoBudgetFields) -> RetrievalBudgetResult<Self> {
        let budget = Self { fields };
        budget.validate()?;
        Ok(budget)
    }

    /// Revalidates fields after decode or before an adapter issues reads.
    pub fn validate(&self) -> RetrievalBudgetResult<()> {
        let f = self.fields;
        if f.max_pages == 0 || f.max_bytes == 0 || f.max_iops == 0 {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::InvalidIoBudget,
            ));
        }
        if f.max_queue_depth == 0 {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::InvalidQueueDepth,
            ));
        }
        if f.max_await_micros == 0 {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::InvalidAwaitBudget,
            ));
        }
        if f.max_read_amplification == 0 {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::InvalidReadAmplificationBound,
            ));
        }
        Ok(())
    }

    /// Returns the validated field bag.
    pub const fn fields(&self) -> IoBudgetFields {
        self.fields
    }

    /// Returns the access mode.
    pub const fn access_mode(&self) -> IoAccessMode {
        self.fields.access_mode
    }

    /// Returns the typed exhaustion reason when any consumed dimension exceeds
    /// its bound, else `None`. Dimensions are checked in the order pages,
    /// bytes, IOPS, await, read-amplification.
    pub fn exhaustion(
        &self,
        consumed_pages: u64,
        consumed_bytes: u64,
        consumed_iops: u64,
        consumed_await_micros: u64,
        measured_read_amplification: u32,
    ) -> Option<ResourceExhaustion> {
        if consumed_pages > self.fields.max_pages {
            return Some(ResourceExhaustion::PageBudgetExceeded);
        }
        if consumed_bytes > self.fields.max_bytes {
            return Some(ResourceExhaustion::ByteBudgetExceeded);
        }
        if consumed_iops > self.fields.max_iops {
            return Some(ResourceExhaustion::IopsExceeded);
        }
        if consumed_await_micros > self.fields.max_await_micros {
            return Some(ResourceExhaustion::AwaitExceeded);
        }
        if measured_read_amplification > self.fields.max_read_amplification {
            return Some(ResourceExhaustion::ReadAmplificationExceeded);
        }
        None
    }

    /// Returns `Err` with the typed exhaustion code when any dimension is
    /// exceeded, else `Ok(())`. Convenience wrapper around [`Self::exhaustion`].
    pub fn check(
        &self,
        consumed_pages: u64,
        consumed_bytes: u64,
        consumed_iops: u64,
        consumed_await_micros: u64,
        measured_read_amplification: u32,
    ) -> RetrievalBudgetResult<()> {
        match self.exhaustion(
            consumed_pages,
            consumed_bytes,
            consumed_iops,
            consumed_await_micros,
            measured_read_amplification,
        ) {
            Some(code) => Err(RetrievalBudgetError::new(code.into())),
            None => Ok(()),
        }
    }

    /// Conservative walking-skeleton defaults.
    pub const fn skeleton_default() -> Self {
        Self {
            fields: IoBudgetFields {
                max_pages: 4_096,
                max_bytes: 16 * super::memory::MIB,
                max_iops: 1_024,
                max_queue_depth: 8,
                max_await_micros: 100_000,
                max_read_amplification: 5 * READ_AMP_DENOMINATOR,
                access_mode: IoAccessMode::Direct,
            },
        }
    }
}

/// Typed exhaustion state surfaced when an I/O budget dimension is exceeded.
///
/// Re-exported at the module root as [`super::ResourceExhaustion`]; it is the
/// single enum covering memory, I/O, concurrency, and deadline exhaustion so a
/// caller has one exhaustive match surface.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResourceExhaustion {
    /// `memory.high` exceeded (reclaim pressure).
    MemoryHighExceeded,
    /// `memory.max` exceeded (hard ceiling).
    MemoryMaxExceeded,
    /// Per-request page-read cap reached.
    PageBudgetExceeded,
    /// Per-request byte-read cap reached.
    ByteBudgetExceeded,
    /// Per-request IOPS cap reached.
    IopsExceeded,
    /// Per-request await-time cap reached.
    AwaitExceeded,
    /// Read-amplification ceiling exceeded.
    ReadAmplificationExceeded,
    /// No free worker slot (concurrency saturated).
    ConcurrencySaturated,
    /// Shared wall-time deadline reached.
    DeadlineExceeded,
}

impl ResourceExhaustion {
    /// Every exhaustion variant, useful for exhaustive contract tests.
    pub const ALL: [Self; 9] = [
        Self::MemoryHighExceeded,
        Self::MemoryMaxExceeded,
        Self::PageBudgetExceeded,
        Self::ByteBudgetExceeded,
        Self::IopsExceeded,
        Self::AwaitExceeded,
        Self::ReadAmplificationExceeded,
        Self::ConcurrencySaturated,
        Self::DeadlineExceeded,
    ];

    /// Stable machine-readable code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::MemoryHighExceeded => "memory_high_exceeded",
            Self::MemoryMaxExceeded => "memory_max_exceeded",
            Self::PageBudgetExceeded => "page_budget_exceeded",
            Self::ByteBudgetExceeded => "byte_budget_exceeded",
            Self::IopsExceeded => "iops_exceeded",
            Self::AwaitExceeded => "await_exceeded",
            Self::ReadAmplificationExceeded => "read_amplification_exceeded",
            Self::ConcurrencySaturated => "concurrency_saturated",
            Self::DeadlineExceeded => "deadline_exceeded",
        }
    }
}

impl From<ResourceExhaustion> for RetrievalBudgetDiagnosticCode {
    fn from(value: ResourceExhaustion) -> Self {
        match value {
            ResourceExhaustion::MemoryHighExceeded => Self::MemoryHighExceeded,
            ResourceExhaustion::MemoryMaxExceeded => Self::MemoryMaxExceeded,
            ResourceExhaustion::PageBudgetExceeded => Self::PageBudgetExceeded,
            ResourceExhaustion::ByteBudgetExceeded => Self::ByteBudgetExceeded,
            ResourceExhaustion::IopsExceeded => Self::IopsExceeded,
            ResourceExhaustion::AwaitExceeded => Self::InvalidAwaitBudget,
            ResourceExhaustion::ReadAmplificationExceeded => Self::ReadAmplificationExceeded,
            ResourceExhaustion::ConcurrencySaturated => Self::ConcurrencySaturated,
            ResourceExhaustion::DeadlineExceeded => Self::DeadlineExceeded,
        }
    }
}
