//! Hard concurrency budgets: active query cap, worker cap, per-category limits.
//!
//! Issue #377 requires bounded concurrent model, storage, and retrieval work
//! so that embedding/reranking cannot evict the entire search working set, and
//! forbids one Tokio thread or allocator arena per shard. This module declares
//! the validated concurrency budget with a shared worker pool and independent
//! per-category sub-limits.
//!
//! Contract only — no live semaphore, no runtime spawn, no thread pool.

use serde::{Deserialize, Serialize};

use super::{RetrievalBudgetDiagnosticCode, RetrievalBudgetError, RetrievalBudgetResult};

/// Named category of concurrent work, each independently sub-bounded.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum WorkCategory {
    /// In-flight retrieval queries (search + fusion + rerank).
    Retrieval,
    /// Storage / I/O work (reads, hydration).
    Storage,
    /// Model inference (embedding, reranking).
    Model,
    /// Background build / compaction / merge.
    Background,
}

impl WorkCategory {
    /// Every work category, useful for exhaustive contract tests.
    pub const ALL: [Self; 4] = [
        Self::Retrieval,
        Self::Storage,
        Self::Model,
        Self::Background,
    ];

    /// Stable machine-readable name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Retrieval => "retrieval",
            Self::Storage => "storage",
            Self::Model => "model",
            Self::Background => "background",
        }
    }
}

/// Field bag used to construct and validate a [`ConcurrencyBudget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConcurrencyBudgetFields {
    /// Maximum concurrently active retrieval queries.
    pub max_active_queries: u16,
    /// Shared worker cap across all categories.
    pub max_workers: u16,
    /// Per-category sub-limits. Each must be `<= max_workers`.
    pub retrieval: u16,
    pub storage: u16,
    pub model: u16,
    pub background: u16,
}

/// A validated hard concurrency budget.
///
/// The shared `max_workers` is the absolute ceiling; each per-category limit
/// may be lower but may never exceed it, so no single category can starve the
/// others or evict the entire working set.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConcurrencyBudget {
    fields: ConcurrencyBudgetFields,
}

impl ConcurrencyBudget {
    /// Constructs a budget only when every cap is positive and no category
    /// limit exceeds the shared worker cap.
    pub fn new(fields: ConcurrencyBudgetFields) -> RetrievalBudgetResult<Self> {
        let budget = Self { fields };
        budget.validate()?;
        Ok(budget)
    }

    /// Revalidates invariants after decode or before an adapter spawns work.
    pub fn validate(&self) -> RetrievalBudgetResult<()> {
        let f = self.fields;
        if f.max_active_queries == 0 || f.max_workers == 0 {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::InvalidConcurrencyBudget,
            ));
        }
        if [f.retrieval, f.storage, f.model, f.background].contains(&0) {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::InvalidConcurrencyBudget,
            ));
        }
        if f.retrieval > f.max_workers
            || f.storage > f.max_workers
            || f.model > f.max_workers
            || f.background > f.max_workers
        {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::CategoryLimitExceedsWorkers,
            ));
        }
        Ok(())
    }

    /// Returns the validated field bag.
    pub const fn fields(&self) -> ConcurrencyBudgetFields {
        self.fields
    }

    /// Returns the per-category sub-limit.
    pub const fn category_limit(&self, category: WorkCategory) -> u16 {
        match category {
            WorkCategory::Retrieval => self.fields.retrieval,
            WorkCategory::Storage => self.fields.storage,
            WorkCategory::Model => self.fields.model,
            WorkCategory::Background => self.fields.background,
        }
    }

    /// Returns `Ok(())` when the given category still has a free slot and the
    /// shared worker pool has headroom, else a typed saturation error.
    ///
    /// `category_active` is the current in-flight count for `category`;
    /// `total_active` is the sum across all categories.
    pub fn admit(
        &self,
        category: WorkCategory,
        category_active: u16,
        total_active: u16,
    ) -> RetrievalBudgetResult<()> {
        let limit = self.category_limit(category);
        if category_active >= limit {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::ConcurrencySaturated,
            ));
        }
        if total_active >= self.fields.max_workers {
            return Err(RetrievalBudgetError::new(
                RetrievalBudgetDiagnosticCode::ConcurrencySaturated,
            ));
        }
        Ok(())
    }

    /// Conservative walking-skeleton defaults.
    pub const fn skeleton_default() -> Self {
        Self {
            fields: ConcurrencyBudgetFields {
                max_active_queries: 32,
                max_workers: 16,
                retrieval: 8,
                storage: 8,
                model: 4,
                background: 2,
            },
        }
    }
}
