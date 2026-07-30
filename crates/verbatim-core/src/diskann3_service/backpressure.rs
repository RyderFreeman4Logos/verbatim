//! Fixed serving admission, bounded queue deadlines, retry budget preservation, and worker isolation types.

use crate::search_planner::SearchBudget;

use super::{DiskAnn3ServiceDiagnosticCode, DiskAnn3ServiceError, DiskAnn3ServiceResult};

/// Health-derived circuit state. No runtime circuit breaker is implemented in this slice.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CircuitState {
    Closed,
    Open,
}

/// Separate non-serving worker-pool identity; it is not an active query pool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WorkerPool {
    Build,
    Update,
    Compaction,
}

/// Admission bounds. Retry budget must be strictly narrower than original work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackpressureConfig {
    max_active_queries: u32,
    max_queue_depth: u32,
    queue_deadline_micros: u64,
    original_budget: SearchBudget,
    retry_budget: SearchBudget,
    max_tenant_work: u64,
}

impl BackpressureConfig {
    pub fn new(
        max_active_queries: u32,
        max_queue_depth: u32,
        queue_deadline_micros: u64,
        original_budget: SearchBudget,
        retry_budget: SearchBudget,
        max_tenant_work: u64,
    ) -> DiskAnn3ServiceResult<Self> {
        if max_active_queries == 0
            || max_queue_depth == 0
            || queue_deadline_micros == 0
            || max_tenant_work == 0
        {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::InvalidBackpressureConfig,
            ));
        }
        original_budget.validate().map_err(|_| {
            DiskAnn3ServiceError::contract(DiskAnn3ServiceDiagnosticCode::BudgetExceeded)
        })?;
        retry_budget.validate().map_err(|_| {
            DiskAnn3ServiceError::contract(DiskAnn3ServiceDiagnosticCode::BudgetExceeded)
        })?;
        if !retry_budget.is_not_wider_than(&original_budget) || retry_budget == original_budget {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::RetryBudgetReset,
            ));
        }
        Ok(Self {
            max_active_queries,
            max_queue_depth,
            queue_deadline_micros,
            original_budget,
            retry_budget,
            max_tenant_work,
        })
    }

    pub const fn worker_pool_is_isolated(pool: WorkerPool) -> bool {
        matches!(
            pool,
            WorkerPool::Build | WorkerPool::Update | WorkerPool::Compaction
        )
    }

    pub const fn retry_budget(&self) -> SearchBudget {
        self.retry_budget
    }
    pub const fn original_budget(&self) -> SearchBudget {
        self.original_budget
    }
    pub const fn queue_deadline_micros(&self) -> u64 {
        self.queue_deadline_micros
    }
    pub const fn max_tenant_work(&self) -> u64 {
        self.max_tenant_work
    }
}

/// Observed tenant work and queue wait at one pure admission decision.
///
/// The caller supplies the cumulative already-admitted tenant work plus the work
/// it proposes to charge for this request. `queue_wait_micros` is measured by the
/// runtime clock outside this types-only contract.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AdmissionContext<'a> {
    tenant: &'a str,
    active_queries: u32,
    queued_queries: u32,
    observed_tenant_work: u64,
    charged_tenant_work: u64,
    queue_wait_micros: u64,
}

impl<'a> AdmissionContext<'a> {
    pub fn new(
        tenant: &'a str,
        active_queries: u32,
        queued_queries: u32,
        observed_tenant_work: u64,
        charged_tenant_work: u64,
        queue_wait_micros: u64,
    ) -> DiskAnn3ServiceResult<Self> {
        if tenant.is_empty() {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::InvalidBackpressureConfig,
            ));
        }
        Ok(Self {
            tenant,
            active_queries,
            queued_queries,
            observed_tenant_work,
            charged_tenant_work,
            queue_wait_micros,
        })
    }
}

/// Pure admission evaluator. Callers must cancel outstanding disk work on rejection/cancellation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackpressureGate {
    config: BackpressureConfig,
    circuit: CircuitState,
}

impl BackpressureGate {
    pub const fn new(config: BackpressureConfig, circuit: CircuitState) -> Self {
        Self { config, circuit }
    }

    /// Admits only work within the tenant cap and before the bounded queue deadline.
    pub fn admit(&self, context: AdmissionContext<'_>) -> DiskAnn3ServiceResult<()> {
        if self.circuit == CircuitState::Open {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::CircuitOpen,
            ));
        }
        if context.active_queries >= self.config.max_active_queries {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::ActiveQueryExceeded,
            ));
        }
        if context.queued_queries >= self.config.max_queue_depth {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::QueueExceeded,
            ));
        }
        if context.queue_wait_micros >= self.config.queue_deadline_micros {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::QueueDeadlineExceeded,
            ));
        }
        if context
            .observed_tenant_work
            .checked_add(context.charged_tenant_work)
            .is_none_or(|work| work > self.config.max_tenant_work)
        {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::TenantWorkExceeded,
            ));
        }
        Ok(())
    }
}
