//! Capability discovery and bounded page-cache diagnostics for DiskANN3 adapters.

use super::{
    DiskAnnBackendDiagnosticCode, DiskAnnBackendError, DiskAnnBackendResult, FullQualityGuarantee,
    SearchBudgetBinding, VectorMetric,
};

/// Explicit capability envelope advertised by one adapter instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskAnnCapabilityFields {
    pub supported_metrics: Vec<VectorMetric>,
    pub supports_predicate_aware_search: bool,
    pub supports_top_k: bool,
    pub supports_range_search: bool,
    pub supports_exact_vector_fetch: bool,
    pub supports_batch_upsert: bool,
    pub supports_tombstones: bool,
    pub supports_snapshot_restore: bool,
    pub supports_reproducible_rebuild: bool,
    pub supports_deterministic_shutdown: bool,
    pub max_page_reads: u64,
    pub max_cache_bytes: u64,
    pub max_bytes_read: u64,
    pub full_quality: FullQualityGuarantee,
}

/// Validated immutable adapter capability envelope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiskAnnCapabilities {
    fields: DiskAnnCapabilityFields,
}

impl DiskAnnCapabilities {
    /// Rejects unbounded capability claims and recovery-free adapters.
    pub fn new(fields: DiskAnnCapabilityFields) -> DiskAnnBackendResult<Self> {
        if fields.supported_metrics.is_empty()
            || fields.max_page_reads == 0
            || fields.max_cache_bytes == 0
            || fields.max_bytes_read == 0
            || (!fields.supports_snapshot_restore && !fields.supports_reproducible_rebuild)
        {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidCapabilities,
            ));
        }
        Ok(Self { fields })
    }

    /// Returns the transparent capability discovery fields.
    pub const fn fields(&self) -> &DiskAnnCapabilityFields {
        &self.fields
    }

    /// Rejects an operation budget that exceeds advertised page or byte authority.
    pub fn validate_budget(&self, budget: &SearchBudgetBinding) -> DiskAnnBackendResult<()> {
        let fields = budget.operation_budget().fields();
        if fields.max_ssd_pages > self.fields.max_page_reads
            || fields.max_bytes_read > self.fields.max_bytes_read
        {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::CapabilityBudgetExceeded,
            ));
        }
        Ok(())
    }
}

/// Bounded diagnostic counters for one adapter operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageCacheDiagnosticFields {
    pub page_reads: u64,
    pub bytes_read: u64,
    pub cache_bytes: u64,
    pub cache_hits: u64,
    pub cache_misses: u64,
}

/// Validated page-cache diagnostics that expose counters, never page keys or paths.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageCacheDiagnostics {
    fields: PageCacheDiagnosticFields,
}

impl PageCacheDiagnostics {
    /// Bounds diagnostics by both the request budget and the advertised adapter envelope.
    pub fn new(
        fields: PageCacheDiagnosticFields,
        budget: &SearchBudgetBinding,
        capabilities: &DiskAnnCapabilities,
    ) -> DiskAnnBackendResult<Self> {
        capabilities.validate_budget(budget)?;
        let operation = budget.operation_budget().fields();
        if fields.page_reads > operation.max_ssd_pages
            || fields.page_reads > capabilities.fields.max_page_reads
            || fields.bytes_read > operation.max_bytes_read
            || fields.bytes_read > capabilities.fields.max_bytes_read
            || fields.cache_bytes > capabilities.fields.max_cache_bytes
        {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::PageCacheDiagnosticsExceeded,
            ));
        }
        Ok(Self { fields })
    }

    /// Returns bounded aggregate cache diagnostics only.
    pub const fn fields(&self) -> PageCacheDiagnosticFields {
        self.fields
    }
}
