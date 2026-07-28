//! Backend capability discovery required before safe path selection.

use super::{SearchBudget, SearchPlannerDiagnosticCode, SearchPlannerError, SearchPlannerResult};

/// Vector similarity metric exposed by a retrieval backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum VectorMetric {
    /// Cosine similarity.
    Cosine,
    /// Dot-product similarity.
    DotProduct,
    /// Squared Euclidean distance.
    SquaredEuclidean,
}

/// Cold-cache behavior declared by a backend.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColdCacheBehavior {
    /// Cold-cache work remains measurable and inside declared caps.
    Bounded,
    /// The backend is safe only after a warm-cache precondition.
    RequiresWarmCache,
    /// The backend cannot make a cold-cache safety declaration.
    Unsupported,
}

/// Storage tier in which a backend normally serves candidate data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MemoryTierBehavior {
    /// Candidates are served from SSD-backed structures.
    SsdResident,
    /// Candidates are served from memory-resident structures.
    MemoryResident,
    /// Candidates may use a bounded hybrid of memory and SSD.
    Hybrid,
}

/// Complete capability-discovery record for one backend implementation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendCapabilityFields {
    /// Supported similarity metrics.
    pub supported_metrics: Vec<VectorMetric>,
    /// Inclusive minimum supported vector dimension.
    pub min_dimension: u32,
    /// Inclusive maximum supported vector dimension.
    pub max_dimension: u32,
    /// Whether exact sequential SIMD scan is available.
    pub exact_simd_scan: bool,
    /// Whether predicates apply before ranking.
    pub supports_pre_rank_predicates: bool,
    /// Whether predicates apply during graph traversal.
    pub supports_in_traversal_predicates: bool,
    /// Whether predicates apply only after candidate generation.
    pub supports_post_filter_predicates: bool,
    /// Whether a request can span multiple sources.
    pub supports_multi_source: bool,
    /// Whether collection predicates are supported.
    pub supports_collection: bool,
    /// Whether tenant predicates are supported.
    pub supports_tenant: bool,
    /// Whether strict ACL predicates are supported.
    pub supports_acl: bool,
    /// Whether stable pagination is supported.
    pub supports_pagination: bool,
    /// Whether explicit range or enumeration queries are supported.
    pub supports_range: bool,
    /// Whether updates are represented by the capability.
    pub supports_updates: bool,
    /// Whether deletes are represented by the capability.
    pub supports_deletes: bool,
    /// Whether results bind to an immutable generation.
    pub binds_generation: bool,
    /// Maximum safe aggregate candidate count.
    pub max_safe_candidates: u32,
    /// Maximum safe SSD page count.
    pub max_safe_pages: u64,
    /// Maximum safe byte count.
    pub max_safe_bytes: u64,
    /// Maximum safe CPU time in microseconds.
    pub max_safe_cpu_micros: u64,
    /// Maximum safe implementation-defined work units.
    pub max_safe_work_units: u64,
    /// Maximum safe concurrently active stages.
    pub max_safe_concurrent_stages: u16,
    /// Whether SSD page reads are reported.
    pub reports_pages_read: bool,
    /// Whether bytes read are reported.
    pub reports_bytes_read: bool,
    /// Whether CPU time is reported.
    pub reports_cpu_micros: bool,
    /// Whether work units are reported.
    pub reports_work_units: bool,
    /// Cold-cache behavior.
    pub cold_cache_behavior: ColdCacheBehavior,
    /// Memory-tier behavior.
    pub memory_tier_behavior: MemoryTierBehavior,
}

/// Validated, backend-neutral capability discovery result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackendCapability {
    fields: BackendCapabilityFields,
}

impl BackendCapability {
    /// Constructs a capability record only when its dimensional and safe-limit facts are usable.
    pub fn new(fields: BackendCapabilityFields) -> SearchPlannerResult<Self> {
        let capability = Self { fields };
        capability.validate()?;
        Ok(capability)
    }

    /// Revalidates basic discovery facts before a request is planned.
    pub fn validate(&self) -> SearchPlannerResult<()> {
        let fields = &self.fields;
        if fields.supported_metrics.is_empty()
            || fields.min_dimension == 0
            || fields.min_dimension > fields.max_dimension
            || fields.max_safe_candidates == 0
            || fields.max_safe_pages == 0
            || fields.max_safe_bytes == 0
            || fields.max_safe_cpu_micros == 0
            || fields.max_safe_work_units == 0
            || fields.max_safe_concurrent_stages == 0
        {
            Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::InvalidCapability,
            ))
        } else {
            Ok(())
        }
    }

    /// Returns the complete declared discovery record.
    pub const fn fields(&self) -> &BackendCapabilityFields {
        &self.fields
    }

    /// Returns whether a metric is supported.
    pub fn supports_metric(&self, metric: VectorMetric) -> bool {
        self.fields.supported_metrics.contains(&metric)
    }

    /// Returns whether a vector dimension is supported.
    pub const fn supports_dimension(&self, dimension: u32) -> bool {
        dimension >= self.fields.min_dimension && dimension <= self.fields.max_dimension
    }

    /// Returns whether predicate-aware DiskANN3-style traversal is supported.
    pub const fn supports_predicate_aware_diskann3(&self) -> bool {
        self.fields.supports_in_traversal_predicates
    }

    pub(crate) const fn supports_exact_simd_scan(&self) -> bool {
        self.fields.exact_simd_scan
    }

    pub(crate) const fn supports_strict_pre_rank_predicates(&self) -> bool {
        self.fields.supports_pre_rank_predicates || self.fields.supports_in_traversal_predicates
    }

    pub(crate) const fn supports_exhaustive_enumeration(&self) -> bool {
        self.fields.supports_range && self.fields.exact_simd_scan
    }

    pub(crate) const fn binds_generation(&self) -> bool {
        self.fields.binds_generation
    }

    pub(crate) fn validate_budget(&self, budget: &SearchBudget) -> SearchPlannerResult<()> {
        self.validate()?;
        let limits = budget.fields();
        if budget.total_candidate_limit()? > self.fields.max_safe_candidates
            || limits.max_ssd_pages > self.fields.max_safe_pages
            || limits.max_bytes_read > self.fields.max_safe_bytes
            || limits.max_cpu_micros > self.fields.max_safe_cpu_micros
            || limits.max_work_units > self.fields.max_safe_work_units
            || limits.max_concurrent_stages > self.fields.max_safe_concurrent_stages
        {
            Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::CapabilityLimitExceeded,
            ))
        } else {
            Ok(())
        }
    }

    pub(crate) const fn reports_required_work(&self) -> bool {
        self.fields.reports_pages_read
            && self.fields.reports_bytes_read
            && self.fields.reports_cpu_micros
            && self.fields.reports_work_units
    }
}
