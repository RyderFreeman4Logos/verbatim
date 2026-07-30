//! Fixed serving admission, bounded queue, retry budget preservation, and worker isolation types.

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
    original_budget: SearchBudget,
    retry_budget: SearchBudget,
    max_tenant_work: u64,
}

impl BackpressureConfig {
    pub fn new(
        max_active_queries: u32,
        max_queue_depth: u32,
        original_budget: SearchBudget,
        retry_budget: SearchBudget,
        max_tenant_work: u64,
    ) -> DiskAnn3ServiceResult<Self> {
        if max_active_queries == 0 || max_queue_depth == 0 || max_tenant_work == 0 {
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

    pub fn admit(
        &self,
        tenant: &str,
        active_queries: u32,
        queued_queries: u32,
    ) -> DiskAnn3ServiceResult<()> {
        if tenant.is_empty() {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::InvalidBackpressureConfig,
            ));
        }
        if self.circuit == CircuitState::Open {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::CircuitOpen,
            ));
        }
        if active_queries >= self.config.max_active_queries {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::ActiveQueryExceeded,
            ));
        }
        if queued_queries >= self.config.max_queue_depth {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::QueueExceeded,
            ));
        }
        Ok(())
    }
}
