//! Test-facing SQL statement accounting for bounded hydration batches.

use serde::{Deserialize, Serialize};

use super::{OverfetchError, OverfetchResult};

/// One authoritative-store batch required for complete result hydration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HydrationBatchKind {
    ChunkHeaders,
    ChunkBodies,
    ParentLinks,
    ChunkEvidenceLinks,
    EvidenceUnits,
}

impl HydrationBatchKind {
    /// Every batch kind a complete hydration adapter must account for.
    pub const ALL: [Self; 5] = [
        Self::ChunkHeaders,
        Self::ChunkBodies,
        Self::ParentLinks,
        Self::ChunkEvidenceLinks,
        Self::EvidenceUnits,
    ];

    const fn index(self) -> usize {
        match self {
            Self::ChunkHeaders => 0,
            Self::ChunkBodies => 1,
            Self::ParentLinks => 2,
            Self::ChunkEvidenceLinks => 3,
            Self::EvidenceUnits => 4,
        }
    }
}

/// Per-query SQL statement counter for deterministic N+1 regression tests.
///
/// A storage adapter records ordinary statements with [`Self::record_statement`]
/// and required hydration batches with [`Self::record_hydration_batch`]. Repeating
/// a required batch kind is a deterministic N+1 violation even when a broad total
/// statement cap would otherwise hide it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StatementCountInstrumentation {
    max_statements: u32,
    observed_statements: u32,
    recorded_hydration_batches: [bool; 5],
}

impl StatementCountInstrumentation {
    pub fn new(max_statements: u32) -> OverfetchResult<Self> {
        if max_statements == 0 {
            return Err(OverfetchError::BudgetExceeded);
        }
        Ok(Self {
            max_statements,
            observed_statements: 0,
            recorded_hydration_batches: [false; 5],
        })
    }

    pub const fn max_statements(&self) -> u32 {
        self.max_statements
    }

    pub const fn observed_statements(&self) -> u32 {
        self.observed_statements
    }

    /// Rejects counter state reused from an earlier retrieval.
    pub(crate) fn assert_fresh(&self) -> OverfetchResult<()> {
        if self.observed_statements != 0 || self.observed_hydration_batches() != 0 {
            return Err(OverfetchError::NPlusOneDetected);
        }
        Ok(())
    }

    /// Records any one SQL statement and rejects a request that crosses its cap.
    pub fn record_statement(&mut self) -> OverfetchResult<()> {
        let next = self
            .observed_statements
            .checked_add(1)
            .ok_or(OverfetchError::NPlusOneDetected)?;
        if next > self.max_statements {
            return Err(OverfetchError::NPlusOneDetected);
        }
        self.observed_statements = next;
        Ok(())
    }

    /// Records exactly one statement for one required hydration batch.
    pub fn record_hydration_batch(&mut self, batch: HydrationBatchKind) -> OverfetchResult<()> {
        let index = batch.index();
        if self.recorded_hydration_batches[index] {
            return Err(OverfetchError::NPlusOneDetected);
        }
        self.record_statement()?;
        self.recorded_hydration_batches[index] = true;
        Ok(())
    }

    /// Number of distinct required hydration batches observed in this request.
    pub fn observed_hydration_batches(&self) -> u32 {
        self.recorded_hydration_batches
            .iter()
            .filter(|recorded| **recorded)
            .count() as u32
    }

    /// Verifies every complete-hydration batch has one and only one statement.
    ///
    /// The total must also equal the five required batches: otherwise an
    /// unclassified per-candidate query could fit below a permissive statement
    /// cap and escape the duplicate-batch detector.
    pub fn assert_complete_batched_hydration(&self) -> OverfetchResult<()> {
        let required_batches = HydrationBatchKind::ALL.len() as u32;
        if self.observed_hydration_batches() != required_batches
            || self.observed_statements != required_batches
        {
            return Err(OverfetchError::NPlusOneDetected);
        }
        Ok(())
    }
}
