//! AISAQ-style co-located SSD page-layout contract for the DiskANN3 DataProvider.
//!
//! This module is a **pure contract**: it defines validation and layout
//! boundaries only. It intentionally contains no live SSD I/O, no upstream
//! DiskANN3 binding, no daemon wiring, and no ANN core. It encodes the
//! near-zero-DRAM co-location trade from AISAQ — duplicate a bounded amount of
//! compressed neighbor information beside graph records so candidate codes do
//! not have to remain proportional to corpus size in RAM and search requires
//! fewer unrelated random reads. Disk use rises by a constant factor but stays
//! O(N).
//!
//! See `docs/architecture/aisaq-page-layout.md` for the algorithmic lineage,
//! published-interface basis, and license review note. Refs #374.
//!
//! # Quality rule
//!
//! Original full 4,096-dimensional `float32` vectors are always preserved
//! separately on SSD. Co-located compressed representations are
//! candidate-generation aids, not replacements for exact originals.
//! Full-precision rescoring runs under a separate contract.

mod amplification;
mod checksum;
mod colocation;
mod error;
mod page_size;
mod spec;
mod strategy;

pub use amplification::{ReadAmplificationBound, ReadAmplificationExhaustion};
pub use checksum::{ChecksumPolicy, PageChecksum, CHECKSUM_LEN};
pub use colocation::ColocationRule;
pub use error::{PageLayoutDiagnosticCode, PageLayoutError, PageLayoutResult};
pub use page_size::{PageAlignment, PageSize, MAX_PAGE_SIZE_BYTES, MIN_PAGE_SIZE_BYTES};
pub use spec::{PageLayoutSpec, PageLayoutSpecFields};
pub use strategy::{
    PageLayoutStrategy, COLOCATION_REDUNDANCY_CEILING_PERFORMANCE,
    COLOCATION_REDUNDANCY_CEILING_SCALE,
};

/// Contract schema version for the AISAQ page-layout boundary.
pub const AISAQ_PAGE_LAYOUT_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
#[path = "tests.rs"]
mod page_layout_tests;
