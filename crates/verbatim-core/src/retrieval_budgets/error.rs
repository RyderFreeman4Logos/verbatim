//! Closed, fail-closed, diagnostic-code-only failures for retrieval budgets.
//!
//! No variant retains a caller-controlled identifier, cgroup path, payload, or
//! secret. [`RetrievalBudgetError`] renders only a stable diagnostic-code
//! string, so sensitive runtime state can never escape through `Debug`/`Display`.

use std::error::Error;
use std::fmt;

/// Result alias for retrieval-budget contract operations.
pub type RetrievalBudgetResult<T> = Result<T, RetrievalBudgetError>;

/// Closed diagnostic taxonomy for hard retrieval resource budgets.
///
/// Variants cover every independent budget dimension named in issue #377:
/// memory, page-cache, SSD-I/O, and concurrency, plus the typed exhaustion
/// states a request must surface rather than silently truncating.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RetrievalBudgetDiagnosticCode {
    /// A memory budget field was zero or structurally invalid.
    InvalidMemoryBudget,
    /// `memory.high` fell at or above `memory.max` (high must be strictly lower).
    MemoryHighNotBelowMax,
    /// `memory.max` was below the minimum admissible profile floor.
    MemoryMaxBelowFloor,
    /// The effective `memory.high` threshold was exceeded.
    MemoryHighExceeded,
    /// The hard `memory.max` ceiling was exceeded.
    MemoryMaxExceeded,
    /// A corpus-proportional cache capacity was zero or unbounded.
    InvalidCacheCapacity,
    /// A cache capacity would admit no entries at the declared entry size.
    CacheCapacityBelowOneEntry,
    /// An I/O budget field (pages, bytes, IOPS, queue depth, await) was invalid.
    InvalidIoBudget,
    /// The per-request page-read cap was reached.
    PageBudgetExceeded,
    /// The per-request byte-read cap was reached.
    ByteBudgetExceeded,
    /// The per-request IOPS cap was reached.
    IopsExceeded,
    /// The configured queue depth was zero.
    InvalidQueueDepth,
    /// The await-time budget was zero.
    InvalidAwaitBudget,
    /// The read-amplification ceiling was zero.
    InvalidReadAmplificationBound,
    /// The measured read amplification exceeded the declared ceiling.
    ReadAmplificationExceeded,
    /// A concurrency budget field (active/worker/category) was zero.
    InvalidConcurrencyBudget,
    /// A per-category limit exceeded the shared worker cap.
    CategoryLimitExceedsWorkers,
    /// No free worker slot was available (saturated).
    ConcurrencySaturated,
    /// A deadline (wall-time) budget was zero or already elapsed.
    InvalidDeadline,
    /// The shared wall-time deadline was reached.
    DeadlineExceeded,
    /// A process-isolation field (cgroup path, CPU/IO priority) was invalid.
    InvalidProcessIsolation,
    /// An account was asked to charge more than its remaining headroom.
    AccountOverdrawn,
    /// A derived budget widened a caller-provided hard cap.
    BudgetWidened,
    /// Two budget profiles could not be combined consistently.
    IncompatibleProfiles,
}

impl RetrievalBudgetDiagnosticCode {
    /// Every closed diagnostic code, useful for exhaustive contract tests.
    pub const ALL: [Self; 24] = [
        Self::InvalidMemoryBudget,
        Self::MemoryHighNotBelowMax,
        Self::MemoryMaxBelowFloor,
        Self::MemoryHighExceeded,
        Self::MemoryMaxExceeded,
        Self::InvalidCacheCapacity,
        Self::CacheCapacityBelowOneEntry,
        Self::InvalidIoBudget,
        Self::PageBudgetExceeded,
        Self::ByteBudgetExceeded,
        Self::IopsExceeded,
        Self::InvalidQueueDepth,
        Self::InvalidAwaitBudget,
        Self::InvalidReadAmplificationBound,
        Self::ReadAmplificationExceeded,
        Self::InvalidConcurrencyBudget,
        Self::CategoryLimitExceedsWorkers,
        Self::ConcurrencySaturated,
        Self::InvalidDeadline,
        Self::DeadlineExceeded,
        Self::InvalidProcessIsolation,
        Self::AccountOverdrawn,
        Self::BudgetWidened,
        Self::IncompatibleProfiles,
    ];

    /// Stable machine-readable diagnostic code without caller-controlled data.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidMemoryBudget => "invalid_memory_budget",
            Self::MemoryHighNotBelowMax => "memory_high_not_below_max",
            Self::MemoryMaxBelowFloor => "memory_max_below_floor",
            Self::MemoryHighExceeded => "memory_high_exceeded",
            Self::MemoryMaxExceeded => "memory_max_exceeded",
            Self::InvalidCacheCapacity => "invalid_cache_capacity",
            Self::CacheCapacityBelowOneEntry => "cache_capacity_below_one_entry",
            Self::InvalidIoBudget => "invalid_io_budget",
            Self::PageBudgetExceeded => "page_budget_exceeded",
            Self::ByteBudgetExceeded => "byte_budget_exceeded",
            Self::IopsExceeded => "iops_exceeded",
            Self::InvalidQueueDepth => "invalid_queue_depth",
            Self::InvalidAwaitBudget => "invalid_await_budget",
            Self::InvalidReadAmplificationBound => "invalid_read_amplification_bound",
            Self::ReadAmplificationExceeded => "read_amplification_exceeded",
            Self::InvalidConcurrencyBudget => "invalid_concurrency_budget",
            Self::CategoryLimitExceedsWorkers => "category_limit_exceeds_workers",
            Self::ConcurrencySaturated => "concurrency_saturated",
            Self::InvalidDeadline => "invalid_deadline",
            Self::DeadlineExceeded => "deadline_exceeded",
            Self::InvalidProcessIsolation => "invalid_process_isolation",
            Self::AccountOverdrawn => "account_overdrawn",
            Self::BudgetWidened => "budget_widened",
            Self::IncompatibleProfiles => "incompatible_profiles",
        }
    }
}

/// A retrieval-budget contract failure that retains only a stable diagnostic code.
///
/// No payload — the redacted `Debug` and `Display` implementations render only
/// the code string, so cgroup paths, memory figures, cache keys, and any other
/// caller-controlled data can never leak.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct RetrievalBudgetError {
    code: RetrievalBudgetDiagnosticCode,
}

impl RetrievalBudgetError {
    /// Constructs a closed error from an internal diagnostic code.
    pub(crate) const fn new(code: RetrievalBudgetDiagnosticCode) -> Self {
        Self { code }
    }

    /// Returns the closed diagnostic code without any caller-controlled detail.
    pub const fn diagnostic_code(self) -> RetrievalBudgetDiagnosticCode {
        self.code
    }
}

impl fmt::Debug for RetrievalBudgetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "RetrievalBudgetError({})",
            self.diagnostic_code().as_str()
        )
    }
}

impl fmt::Display for RetrievalBudgetError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "retrieval-budget.{}",
            self.diagnostic_code().as_str()
        )
    }
}

impl Error for RetrievalBudgetError {}
