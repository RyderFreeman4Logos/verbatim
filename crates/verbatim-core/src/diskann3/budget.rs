//! Hard-bounded memory, SSD I/O, concurrency, and stage-output budgets.

use serde::{Deserialize, Serialize};

use super::{RetrievalStage, VectorSearchDiagnosticCode, VectorSearchError, VectorSearchResult};

pub const MEBIBYTE: u64 = 1_024 * 1_024;
/// Architecture hard cap: online peak memory remains bounded below 512 MiB.
pub const MAX_PEAK_MEMORY_BYTES: u64 = 512 * MEBIBYTE;

/// Per-request and global peak-memory caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct MemoryBudget {
    pub per_request_peak_bytes: u64,
    pub global_peak_bytes: u64,
}

impl MemoryBudget {
    pub fn new(per_request_peak_bytes: u64, global_peak_bytes: u64) -> VectorSearchResult<Self> {
        let budget = Self {
            per_request_peak_bytes,
            global_peak_bytes,
        };
        budget.validate()?;
        Ok(budget)
    }

    pub fn validate(&self) -> VectorSearchResult<()> {
        if self.per_request_peak_bytes == 0
            || self.global_peak_bytes == 0
            || self.per_request_peak_bytes > self.global_peak_bytes
            || self.global_peak_bytes > MAX_PEAK_MEMORY_BYTES
        {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::BudgetExceeded,
            ));
        }
        Ok(())
    }
}

/// SSD read budget for one query.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct IoBudget {
    pub max_page_reads: u32,
    pub max_bytes_read: u64,
    pub deadline_ms: u64,
}

impl IoBudget {
    pub fn new(
        max_page_reads: u32,
        max_bytes_read: u64,
        deadline_ms: u64,
    ) -> VectorSearchResult<Self> {
        let budget = Self {
            max_page_reads,
            max_bytes_read,
            deadline_ms,
        };
        budget.validate()?;
        Ok(budget)
    }

    pub fn validate(&self) -> VectorSearchResult<()> {
        if self.max_page_reads == 0 || self.max_bytes_read == 0 || self.deadline_ms == 0 {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::BudgetExceeded,
            ));
        }
        Ok(())
    }
}

/// Per-request and global retrieval concurrency caps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConcurrencyBudget {
    pub per_request: u16,
    pub global: u16,
}

impl ConcurrencyBudget {
    pub fn new(per_request: u16, global: u16) -> VectorSearchResult<Self> {
        let budget = Self {
            per_request,
            global,
        };
        budget.validate()?;
        Ok(budget)
    }

    pub fn validate(&self) -> VectorSearchResult<()> {
        if self.per_request == 0 || self.global == 0 || self.per_request > self.global {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::BudgetExceeded,
            ));
        }
        Ok(())
    }
}

/// Maximum bounded output at each retrieval stage in declaration order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct RetrievalStageBudget {
    caps: [u32; 6],
}

impl RetrievalStageBudget {
    pub fn new(caps: [u32; 6]) -> VectorSearchResult<Self> {
        let budget = Self { caps };
        budget.validate()?;
        Ok(budget)
    }

    pub fn uniform(cap: u32) -> VectorSearchResult<Self> {
        Self::new([cap; 6])
    }

    pub fn validate(&self) -> VectorSearchResult<()> {
        if self.caps.contains(&0) {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::StageOutputExceeded,
            ));
        }
        Ok(())
    }

    pub const fn cap(&self, stage: RetrievalStage) -> u32 {
        self.caps[stage.index()]
    }
}

/// Combined hard search budget, including the output cap before hydration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchBudget {
    memory: MemoryBudget,
    io: IoBudget,
    concurrency: ConcurrencyBudget,
    stage_budget: RetrievalStageBudget,
}

impl SearchBudget {
    pub fn new(
        memory: MemoryBudget,
        io: IoBudget,
        concurrency: ConcurrencyBudget,
        stage_budget: RetrievalStageBudget,
    ) -> VectorSearchResult<Self> {
        let budget = Self {
            memory,
            io,
            concurrency,
            stage_budget,
        };
        budget.validate()?;
        Ok(budget)
    }

    pub const fn stage_budget(&self) -> &RetrievalStageBudget {
        &self.stage_budget
    }

    pub fn validate(&self) -> VectorSearchResult<()> {
        self.memory.validate()?;
        self.io.validate()?;
        self.concurrency.validate()?;
        self.stage_budget.validate()
    }

    pub fn check_usage(&self, usage: BudgetUsage) -> VectorSearchResult<()> {
        self.validate()?;
        if usage.peak_memory_bytes > self.memory.per_request_peak_bytes
            || usage.peak_memory_bytes > self.memory.global_peak_bytes
            || usage.page_reads > self.io.max_page_reads
            || usage.bytes_read > self.io.max_bytes_read
            || usage.elapsed_ms > self.io.deadline_ms
            || usage.request_concurrency > self.concurrency.per_request
            || usage.global_concurrency > self.concurrency.global
        {
            return Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::BudgetExceeded,
            ));
        }
        Ok(())
    }
}

/// Measured request usage that is checked before any result may be returned.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BudgetUsage {
    pub peak_memory_bytes: u64,
    pub page_reads: u32,
    pub bytes_read: u64,
    pub elapsed_ms: u64,
    pub request_concurrency: u16,
    pub global_concurrency: u16,
}
