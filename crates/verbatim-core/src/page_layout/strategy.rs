//! Page-layout strategies inspired by AISAQ's near-zero-DRAM co-location trade.
//!
//! See the issue body and `docs/architecture/aisaq-page-layout.md` for the
//! algorithmic lineage, the published-interface basis, and the license review
//! note. This enum names the three comparable layouts required by the contract
//! and records their per-strategy tradeoffs without performing any SSD I/O.

use super::{PageLayoutDiagnosticCode, PageLayoutError, PageLayoutResult};

/// The three comparable AISAQ-style co-located SSD page-layout strategies.
///
/// All three strategies are contract-only names today: none of them performs
/// live SSD I/O, binds upstream DiskANN3, or grows an O(N) in-RAM code table.
/// They exist so a future provider can select a layout behind the same
/// DiskANN3 and Verbatim contracts and so each tradeoff is documented in code.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum PageLayoutStrategy {
    /// Upstream/reference disk layout: vectors and graph stored separately on
    /// SSD, compressed/PQ candidate codes kept proportional to corpus size in
    /// RAM. Maximally conservative on SSD redundancy; highest DRAM of the
    /// three. Maps to the issue's `standard-diskann` provider variant.
    VectorFirst,
    /// Graph vertex, neighbor IDs, candidate codes, and necessary metadata
    /// arranged to minimize reads, accepting more linear SSD redundancy.
    /// Trades a bounded constant-factor SSD increase for lower DRAM and IOPS.
    /// Maps to the issue's `colocated-performance` provider variant.
    GraphFirst,
    /// Less redundancy and more SSD I/O, targeting the smallest SSD footprint
    /// while still avoiding an O(N) in-RAM code table. Maps to the issue's
    /// `colocated-scale` provider variant.
    ColocatedScale,
}

impl PageLayoutStrategy {
    /// Returns the stable wire name used in layout selection and docs.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::VectorFirst => "vector-first",
            Self::GraphFirst => "graph-first",
            Self::ColocatedScale => "colocated-scale",
        }
    }

    /// Returns `true` when the strategy co-locates candidate codes beside graph
    /// records to reduce unrelated random reads and DRAM residency.
    pub const fn is_colocated(self) -> bool {
        match self {
            Self::VectorFirst => false,
            Self::GraphFirst | Self::ColocatedScale => true,
        }
    }

    /// Returns the documented constant-factor SSD-redundancy ceiling for the
    /// strategy. Disk use remains O(N); this is the documented upper bound on
    /// the multiplier, not a live measurement.
    pub const fn redundancy_factor_ceiling(self) -> u8 {
        match self {
            // Reference layout duplicates nothing beside graph records.
            Self::VectorFirst => 1,
            // Performance layout duplicates a bounded amount of compressed
            // neighbor information beside each graph record.
            Self::GraphFirst => COLOCATION_REDUNDANCY_CEILING_PERFORMANCE,
            // Scale layout duplicates less, targeting the smallest footprint.
            Self::ColocatedScale => COLOCATION_REDUNDANCY_CEILING_SCALE,
        }
    }

    /// Rejects a redundancy factor inconsistent with this strategy's ceiling.
    pub fn validate_redundancy_factor(self, factor: u8) -> PageLayoutResult<()> {
        if factor == 0 {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::InvalidRedundancyFactor,
            ));
        }
        if factor > self.redundancy_factor_ceiling() {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::RedundancyFactorTooLarge,
            ));
        }
        Ok(())
    }
}

/// Documented constant-factor SSD-redundancy ceiling for `GraphFirst`.
///
/// Co-location duplicates a bounded amount of compressed neighbor information
/// beside graph records; this bounds that duplication so disk growth stays
/// linear in N with a documented constant factor.
pub const COLOCATION_REDUNDANCY_CEILING_PERFORMANCE: u8 = 4;

/// Documented constant-factor SSD-redundancy ceiling for `ColocatedScale`.
pub const COLOCATION_REDUNDANCY_CEILING_SCALE: u8 = 2;
