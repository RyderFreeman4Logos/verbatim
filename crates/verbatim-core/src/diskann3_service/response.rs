//! Compact response and bounded work telemetry; evidence hydration stays elsewhere.

use super::{
    DiskAnn3ServiceDiagnosticCode, DiskAnn3ServiceError, DiskAnn3ServiceResult, Generation,
};

/// Whether every required shard completed its bounded work.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CompletionState {
    Complete,
    Partial,
    Cancelled,
}

/// Compact result reference: never carries authoritative evidence text.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CompactSearchResult {
    compact_id: u64,
    raw_score: f32,
}

impl CompactSearchResult {
    pub fn new(compact_id: u64, raw_score: f32) -> DiskAnn3ServiceResult<Self> {
        if compact_id == 0 || !raw_score.is_finite() {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::InvalidResponse,
            ));
        }
        Ok(Self {
            compact_id,
            raw_score,
        })
    }

    pub const fn compact_id(&self) -> u64 {
        self.compact_id
    }
    pub const fn raw_score(&self) -> f32 {
        self.raw_score
    }
}

/// Aggregate bounded resource consumption. It is telemetry, not a page-cache claim.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WorkTelemetry {
    ssd_pages: u64,
    bytes_read: u64,
    cpu_micros: u64,
    work_units: u64,
}

impl WorkTelemetry {
    pub fn new(
        ssd_pages: u64,
        bytes_read: u64,
        cpu_micros: u64,
        work_units: u64,
    ) -> DiskAnn3ServiceResult<Self> {
        if [ssd_pages, bytes_read, cpu_micros, work_units].contains(&0) {
            return Err(DiskAnn3ServiceError::contract(
                DiskAnn3ServiceDiagnosticCode::InvalidResponse,
            ));
        }
        Ok(Self {
            ssd_pages,
            bytes_read,
            cpu_micros,
            work_units,
        })
    }

    pub const fn ssd_pages(&self) -> u64 {
        self.ssd_pages
    }
    pub const fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
    pub const fn cpu_micros(&self) -> u64 {
        self.cpu_micros
    }
    pub const fn work_units(&self) -> u64 {
        self.work_units
    }
}

/// Search response shape for both in-process and remote semantic adapters.
#[derive(Debug, Clone, PartialEq)]
pub struct SearchResponse {
    results: Vec<CompactSearchResult>,
    generation: Generation,
    completion: CompletionState,
    telemetry: WorkTelemetry,
}

impl SearchResponse {
    pub fn new(
        results: Vec<(u64, f32)>,
        generation: Generation,
        completion: CompletionState,
        telemetry: WorkTelemetry,
    ) -> DiskAnn3ServiceResult<Self> {
        let results = results
            .into_iter()
            .map(|(id, score)| CompactSearchResult::new(id, score))
            .collect::<DiskAnn3ServiceResult<Vec<_>>>()?;
        Ok(Self {
            results,
            generation,
            completion,
            telemetry,
        })
    }

    pub fn results(&self) -> &[CompactSearchResult] {
        &self.results
    }
    pub const fn generation(&self) -> Generation {
        self.generation
    }
    pub const fn completion(&self) -> CompletionState {
        self.completion
    }
    pub const fn telemetry(&self) -> WorkTelemetry {
        self.telemetry
    }
}
