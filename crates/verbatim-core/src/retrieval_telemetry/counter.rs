//! Overflow-safe candidate, storage, and resource counters for retrieval.

use serde::{Deserialize, Serialize};

use super::{SpanKind, TelemetryDiagnosticCode, TelemetryError, TelemetryResult};

fn checked_sum(current: u64, added: u64) -> TelemetryResult<u64> {
    current.checked_add(added).ok_or(TelemetryError::contract(
        TelemetryDiagnosticCode::CounterOverflow,
    ))
}

fn add_to(current: &mut u64, added: u64) -> TelemetryResult<()> {
    *current = checked_sum(*current, added)?;
    Ok(())
}

/// Requested and returned K values for one fixed [`SpanKind`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageCandidateCounters {
    requested_k: u64,
    returned_k: u64,
}

impl StageCandidateCounters {
    /// Requested K accumulated for this fixed stage.
    pub const fn requested_k(self) -> u64 {
        self.requested_k
    }
    /// Returned K accumulated for this fixed stage.
    pub const fn returned_k(self) -> u64 {
        self.returned_k
    }
}

/// Bounded work/candidate counters for a retrieval run.
///
/// Per-stage K values live in a fixed array keyed by [`SpanKind`], so callers
/// cannot introduce a high-cardinality stage label.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CandidateCounters {
    stages: [StageCandidateCounters; SpanKind::COUNT],
    visited: u64,
    evaluated: u64,
    filtered: u64,
    rejected: u64,
    fused: u64,
    reranked: u64,
    hydrated: u64,
}

impl CandidateCounters {
    /// Creates an empty fixed-size candidate ledger.
    pub const fn new() -> Self {
        Self {
            stages: [StageCandidateCounters {
                requested_k: 0,
                returned_k: 0,
            }; SpanKind::COUNT],
            visited: 0,
            evaluated: 0,
            filtered: 0,
            rejected: 0,
            fused: 0,
            reranked: 0,
            hydrated: 0,
        }
    }
    /// Returns K counters for one closed pipeline stage.
    pub const fn stage(self, kind: SpanKind) -> StageCandidateCounters {
        self.stages[kind.as_index()]
    }
    /// Returns requested K for one closed pipeline stage.
    pub const fn requested_k(self, kind: SpanKind) -> u64 {
        self.stage(kind).requested_k()
    }
    /// Returns returned K for one closed pipeline stage.
    pub const fn returned_k(self, kind: SpanKind) -> u64 {
        self.stage(kind).returned_k()
    }
    pub const fn visited(self) -> u64 {
        self.visited
    }
    pub const fn evaluated(self) -> u64 {
        self.evaluated
    }
    pub const fn filtered(self) -> u64 {
        self.filtered
    }
    pub const fn rejected(self) -> u64 {
        self.rejected
    }
    pub const fn fused(self) -> u64 {
        self.fused
    }
    pub const fn reranked(self) -> u64 {
        self.reranked
    }
    pub const fn hydrated(self) -> u64 {
        self.hydrated
    }
    /// Adds requested K for a fixed stage.
    pub fn add_requested_k(&mut self, kind: SpanKind, value: u64) -> TelemetryResult<()> {
        add_to(&mut self.stages[kind.as_index()].requested_k, value)
    }
    /// Adds returned K for a fixed stage.
    pub fn add_returned_k(&mut self, kind: SpanKind, value: u64) -> TelemetryResult<()> {
        add_to(&mut self.stages[kind.as_index()].returned_k, value)
    }

    pub fn add_visited(&mut self, value: u64) -> TelemetryResult<()> {
        add_to(&mut self.visited, value)
    }

    pub fn add_evaluated(&mut self, value: u64) -> TelemetryResult<()> {
        add_to(&mut self.evaluated, value)
    }

    pub fn add_filtered(&mut self, value: u64) -> TelemetryResult<()> {
        add_to(&mut self.filtered, value)
    }

    pub fn add_rejected(&mut self, value: u64) -> TelemetryResult<()> {
        add_to(&mut self.rejected, value)
    }

    pub fn add_fused(&mut self, value: u64) -> TelemetryResult<()> {
        add_to(&mut self.fused, value)
    }

    pub fn add_reranked(&mut self, value: u64) -> TelemetryResult<()> {
        add_to(&mut self.reranked, value)
    }

    pub fn add_hydrated(&mut self, value: u64) -> TelemetryResult<()> {
        add_to(&mut self.hydrated, value)
    }
    /// Returns a merged ledger or fails before mutating either input.
    pub fn checked_add(self, other: Self) -> TelemetryResult<Self> {
        let mut combined = self;
        for kind in SpanKind::ALL {
            let index = kind.as_index();
            combined.stages[index].requested_k = checked_sum(
                self.stages[index].requested_k,
                other.stages[index].requested_k,
            )?;
            combined.stages[index].returned_k = checked_sum(
                self.stages[index].returned_k,
                other.stages[index].returned_k,
            )?;
        }
        combined.visited = checked_sum(self.visited, other.visited)?;
        combined.evaluated = checked_sum(self.evaluated, other.evaluated)?;
        combined.filtered = checked_sum(self.filtered, other.filtered)?;
        combined.rejected = checked_sum(self.rejected, other.rejected)?;
        combined.fused = checked_sum(self.fused, other.fused)?;
        combined.reranked = checked_sum(self.reranked, other.reranked)?;
        combined.hydrated = checked_sum(self.hydrated, other.hydrated)?;
        Ok(combined)
    }
    /// Merges another ledger atomically after every addition has been checked.
    pub fn add_assign_checked(&mut self, other: Self) -> TelemetryResult<()> {
        *self = self.checked_add(other)?;
        Ok(())
    }
}

/// Closed storage access modes; they are counted rather than used as labels.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StorageAccessMode {
    Direct,
    Buffered,
    Mmap,
}

impl StorageAccessMode {
    const COUNT: usize = 3;
    /// Stable low-cardinality access-mode name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::Buffered => "buffered",
            Self::Mmap => "mmap",
        }
    }

    const fn as_index(self) -> usize {
        self as usize
    }
}

/// Storage and cache work counters for one retrieval run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageCounters {
    sql_statements: u64,
    rows_read: u64,
    bytes_read: u64,
    graph_pages_read: u64,
    graph_bytes_read: u64,
    vector_pages_read: u64,
    vector_bytes_read: u64,
    filter_pages_read: u64,
    filter_bytes_read: u64,
    cache_hits: u64,
    cache_misses: u64,
    cache_evictions: u64,
    access_mode_operations: [u64; StorageAccessMode::COUNT],
    major_faults: u64,
    minor_faults: u64,
}

impl StorageCounters {
    /// Creates an empty storage ledger.
    pub const fn new() -> Self {
        Self {
            sql_statements: 0,
            rows_read: 0,
            bytes_read: 0,
            graph_pages_read: 0,
            graph_bytes_read: 0,
            vector_pages_read: 0,
            vector_bytes_read: 0,
            filter_pages_read: 0,
            filter_bytes_read: 0,
            cache_hits: 0,
            cache_misses: 0,
            cache_evictions: 0,
            access_mode_operations: [0; StorageAccessMode::COUNT],
            major_faults: 0,
            minor_faults: 0,
        }
    }
    pub const fn sql_statements(self) -> u64 {
        self.sql_statements
    }
    pub const fn rows_read(self) -> u64 {
        self.rows_read
    }
    pub const fn bytes_read(self) -> u64 {
        self.bytes_read
    }
    pub const fn graph_pages_read(self) -> u64 {
        self.graph_pages_read
    }
    pub const fn graph_bytes_read(self) -> u64 {
        self.graph_bytes_read
    }
    pub const fn vector_pages_read(self) -> u64 {
        self.vector_pages_read
    }
    pub const fn vector_bytes_read(self) -> u64 {
        self.vector_bytes_read
    }
    pub const fn filter_pages_read(self) -> u64 {
        self.filter_pages_read
    }
    pub const fn filter_bytes_read(self) -> u64 {
        self.filter_bytes_read
    }
    pub const fn cache_hits(self) -> u64 {
        self.cache_hits
    }
    pub const fn cache_misses(self) -> u64 {
        self.cache_misses
    }
    pub const fn cache_evictions(self) -> u64 {
        self.cache_evictions
    }
    /// Returns operation count for one closed I/O access mode.
    pub const fn access_mode_operations(self, mode: StorageAccessMode) -> u64 {
        self.access_mode_operations[mode.as_index()]
    }
    /// Alias for [`Self::access_mode_operations`].
    pub const fn access_mode_count(self, mode: StorageAccessMode) -> u64 {
        self.access_mode_operations(mode)
    }
    pub const fn major_faults(self) -> u64 {
        self.major_faults
    }
    pub const fn minor_faults(self) -> u64 {
        self.minor_faults
    }

    pub fn add_sql_statements(&mut self, value: u64) -> TelemetryResult<()> {
        add_to(&mut self.sql_statements, value)
    }
    /// Records a SQL unit atomically.
    pub fn record_sql(&mut self, statements: u64, rows: u64, bytes: u64) -> TelemetryResult<()> {
        let sql_statements = checked_sum(self.sql_statements, statements)?;
        let rows_read = checked_sum(self.rows_read, rows)?;
        let bytes_read = checked_sum(self.bytes_read, bytes)?;
        self.sql_statements = sql_statements;
        self.rows_read = rows_read;
        self.bytes_read = bytes_read;
        Ok(())
    }

    pub fn record_graph_read(&mut self, pages: u64, bytes: u64) -> TelemetryResult<()> {
        let graph_pages_read = checked_sum(self.graph_pages_read, pages)?;
        let graph_bytes_read = checked_sum(self.graph_bytes_read, bytes)?;
        self.graph_pages_read = graph_pages_read;
        self.graph_bytes_read = graph_bytes_read;
        Ok(())
    }

    pub fn record_vector_read(&mut self, pages: u64, bytes: u64) -> TelemetryResult<()> {
        let vector_pages_read = checked_sum(self.vector_pages_read, pages)?;
        let vector_bytes_read = checked_sum(self.vector_bytes_read, bytes)?;
        self.vector_pages_read = vector_pages_read;
        self.vector_bytes_read = vector_bytes_read;
        Ok(())
    }

    pub fn record_filter_read(&mut self, pages: u64, bytes: u64) -> TelemetryResult<()> {
        let filter_pages_read = checked_sum(self.filter_pages_read, pages)?;
        let filter_bytes_read = checked_sum(self.filter_bytes_read, bytes)?;
        self.filter_pages_read = filter_pages_read;
        self.filter_bytes_read = filter_bytes_read;
        Ok(())
    }

    pub fn record_cache(&mut self, hits: u64, misses: u64, evictions: u64) -> TelemetryResult<()> {
        let cache_hits = checked_sum(self.cache_hits, hits)?;
        let cache_misses = checked_sum(self.cache_misses, misses)?;
        let cache_evictions = checked_sum(self.cache_evictions, evictions)?;
        self.cache_hits = cache_hits;
        self.cache_misses = cache_misses;
        self.cache_evictions = cache_evictions;
        Ok(())
    }
    /// Records operations under one closed storage access mode.
    pub fn record_access_mode(
        &mut self,
        mode: StorageAccessMode,
        operations: u64,
    ) -> TelemetryResult<()> {
        add_to(
            &mut self.access_mode_operations[mode.as_index()],
            operations,
        )
    }

    pub fn record_page_faults(&mut self, major: u64, minor: u64) -> TelemetryResult<()> {
        let major_faults = checked_sum(self.major_faults, major)?;
        let minor_faults = checked_sum(self.minor_faults, minor)?;
        self.major_faults = major_faults;
        self.minor_faults = minor_faults;
        Ok(())
    }
    /// Returns a fully checked merged storage ledger.
    pub fn checked_add(self, other: Self) -> TelemetryResult<Self> {
        let mut combined = self;
        combined.sql_statements = checked_sum(self.sql_statements, other.sql_statements)?;
        combined.rows_read = checked_sum(self.rows_read, other.rows_read)?;
        combined.bytes_read = checked_sum(self.bytes_read, other.bytes_read)?;
        combined.graph_pages_read = checked_sum(self.graph_pages_read, other.graph_pages_read)?;
        combined.graph_bytes_read = checked_sum(self.graph_bytes_read, other.graph_bytes_read)?;
        combined.vector_pages_read = checked_sum(self.vector_pages_read, other.vector_pages_read)?;
        combined.vector_bytes_read = checked_sum(self.vector_bytes_read, other.vector_bytes_read)?;
        combined.filter_pages_read = checked_sum(self.filter_pages_read, other.filter_pages_read)?;
        combined.filter_bytes_read = checked_sum(self.filter_bytes_read, other.filter_bytes_read)?;
        combined.cache_hits = checked_sum(self.cache_hits, other.cache_hits)?;
        combined.cache_misses = checked_sum(self.cache_misses, other.cache_misses)?;
        combined.cache_evictions = checked_sum(self.cache_evictions, other.cache_evictions)?;
        for index in 0..StorageAccessMode::COUNT {
            combined.access_mode_operations[index] = checked_sum(
                self.access_mode_operations[index],
                other.access_mode_operations[index],
            )?;
        }
        combined.major_faults = checked_sum(self.major_faults, other.major_faults)?;
        combined.minor_faults = checked_sum(self.minor_faults, other.minor_faults)?;
        Ok(combined)
    }
    /// Merges another storage ledger atomically.
    pub fn add_assign_checked(&mut self, other: Self) -> TelemetryResult<()> {
        *self = self.checked_add(other)?;
        Ok(())
    }
}

/// SSD and CPU resource counters for one retrieval run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResourceCounters {
    ssd_operations: u64,
    ssd_iops: u64,
    queue_depth: u64,
    ssd_wait_micros: u64,
    cpu_time_micros: u64,
}

impl ResourceCounters {
    /// Creates an empty resource ledger.
    pub const fn new() -> Self {
        Self {
            ssd_operations: 0,
            ssd_iops: 0,
            queue_depth: 0,
            ssd_wait_micros: 0,
            cpu_time_micros: 0,
        }
    }
    pub const fn ssd_operations(self) -> u64 {
        self.ssd_operations
    }
    pub const fn ssd_iops(self) -> u64 {
        self.ssd_iops
    }
    pub const fn queue_depth(self) -> u64 {
        self.queue_depth
    }
    pub const fn ssd_wait_micros(self) -> u64 {
        self.ssd_wait_micros
    }
    pub const fn cpu_time_micros(self) -> u64 {
        self.cpu_time_micros
    }

    pub fn add_ssd_operations(&mut self, value: u64) -> TelemetryResult<()> {
        add_to(&mut self.ssd_operations, value)
    }

    pub fn add_ssd_iops(&mut self, value: u64) -> TelemetryResult<()> {
        add_to(&mut self.ssd_iops, value)
    }

    pub fn add_queue_depth(&mut self, value: u64) -> TelemetryResult<()> {
        add_to(&mut self.queue_depth, value)
    }

    pub fn add_ssd_wait_micros(&mut self, value: u64) -> TelemetryResult<()> {
        add_to(&mut self.ssd_wait_micros, value)
    }

    pub fn add_cpu_time_micros(&mut self, value: u64) -> TelemetryResult<()> {
        add_to(&mut self.cpu_time_micros, value)
    }
    /// Adds a complete resource sample atomically after every field is checked.
    pub fn record(
        &mut self,
        ssd_operations: u64,
        ssd_iops: u64,
        queue_depth: u64,
        ssd_wait_micros: u64,
        cpu_time_micros: u64,
    ) -> TelemetryResult<()> {
        let next = self.checked_add(Self {
            ssd_operations,
            ssd_iops,
            queue_depth,
            ssd_wait_micros,
            cpu_time_micros,
        })?;
        *self = next;
        Ok(())
    }
    /// Returns a fully checked merged resource ledger.
    pub fn checked_add(self, other: Self) -> TelemetryResult<Self> {
        Ok(Self {
            ssd_operations: checked_sum(self.ssd_operations, other.ssd_operations)?,
            ssd_iops: checked_sum(self.ssd_iops, other.ssd_iops)?,
            queue_depth: checked_sum(self.queue_depth, other.queue_depth)?,
            ssd_wait_micros: checked_sum(self.ssd_wait_micros, other.ssd_wait_micros)?,
            cpu_time_micros: checked_sum(self.cpu_time_micros, other.cpu_time_micros)?,
        })
    }
    /// Merges another resource ledger atomically.
    pub fn add_assign_checked(&mut self, other: Self) -> TelemetryResult<()> {
        *self = self.checked_add(other)?;
        Ok(())
    }
}

/// Request-local counters exposed by the operating system for one retrieval window.
///
/// Each field is independently optional because a platform may expose thread page
/// faults while withholding procfs storage accounting. Zero is an observed delta;
/// `None` is unavailable.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalResourceCounters {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    major_page_faults: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    minor_page_faults: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    block_input_operations: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    storage_read_bytes: Option<u64>,
}

impl RetrievalResourceCounters {
    pub(crate) const fn observed(
        major_page_faults: Option<u64>,
        minor_page_faults: Option<u64>,
        block_input_operations: Option<u64>,
        storage_read_bytes: Option<u64>,
    ) -> Self {
        Self {
            major_page_faults,
            minor_page_faults,
            block_input_operations,
            storage_read_bytes,
        }
    }

    pub const fn major_page_faults(&self) -> Option<u64> {
        self.major_page_faults
    }

    pub const fn minor_page_faults(&self) -> Option<u64> {
        self.minor_page_faults
    }

    pub const fn block_input_operations(&self) -> Option<u64> {
        self.block_input_operations
    }

    pub const fn storage_read_bytes(&self) -> Option<u64> {
        self.storage_read_bytes
    }

    pub const fn is_available(&self) -> bool {
        self.major_page_faults.is_some()
            || self.minor_page_faults.is_some()
            || self.block_input_operations.is_some()
            || self.storage_read_bytes.is_some()
    }
}
