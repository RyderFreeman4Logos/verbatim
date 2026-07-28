//! Co-location rules: which data sits beside a graph vertex on an SSD page.
//!
//! Co-location is the core of the AISAQ near-zero-DRAM trade: duplicate a
//! bounded amount of compressed neighbor information beside graph records so
//! one page read evaluates more expansion choices. Disk use rises by a
//! constant factor but stays O(N), and candidate codes need not remain
//! proportional to corpus size in RAM. Co-located compressed representations
//! are candidate-generation aids only — original full 4,096-dimensional
//! float32 vectors are always preserved separately (see `quality.rs`).

use super::strategy::PageLayoutStrategy;
use super::{PageLayoutDiagnosticCode, PageLayoutError, PageLayoutResult};

/// Which data is co-located beside a graph vertex on an SSD page, and the
/// accepted linear SSD-redundancy tradeoff.
///
/// Every variant preserves the quality rule: full-precision originals live
/// separately on SSD, and co-located compressed representations are
/// candidate-generation aids, not replacements for exact originals.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ColocationRule {
    /// Nothing is duplicated beside graph records. Candidate codes live in a
    /// separate region (or RAM). The reference, highest-DRAM, lowest-redundancy
    /// arrangement. Compatible only with [`PageLayoutStrategy::VectorFirst`].
    Separated {
        /// Always 1 for the separated reference layout.
        redundancy_factor: u8,
    },
    /// Graph vertex, neighbor IDs, candidate codes, and necessary metadata
    /// arranged to minimize reads. Accepts more linear SSD redundancy.
    /// Compatible with [`PageLayoutStrategy::GraphFirst`].
    FullColocation {
        /// Bounded constant-factor duplication of compressed neighbor codes,
        /// validated against the strategy's redundancy ceiling.
        redundancy_factor: u8,
    },
    /// Less redundancy and more SSD I/O than [`ColocationRule::FullColocation`],
    /// targeting the smallest footprint while still avoiding an O(N) in-RAM
    /// code table. Compatible with [`PageLayoutStrategy::ColocatedScale`].
    PartialColocation {
        /// Bounded constant-factor duplication, no greater than the scale ceiling.
        redundancy_factor: u8,
    },
}

impl ColocationRule {
    /// Returns the documented redundancy factor chosen for this rule.
    pub const fn redundancy_factor(self) -> u8 {
        match self {
            Self::Separated { redundancy_factor } => redundancy_factor,
            Self::FullColocation { redundancy_factor } => redundancy_factor,
            Self::PartialColocation { redundancy_factor } => redundancy_factor,
        }
    }

    /// Returns `true` when candidate codes are co-located beside graph records.
    pub const fn is_colocated(self) -> bool {
        matches!(
            self,
            Self::FullColocation { .. } | Self::PartialColocation { .. }
        )
    }

    /// Rejects a rule whose redundancy factor is zero, exceeds its strategy's
    /// documented ceiling, or is incompatible with the selected strategy.
    ///
    /// Compatibility is fixed: `Separated` pairs only with `VectorFirst`,
    /// `FullColocation` only with `GraphFirst`, and `PartialColocation` only
    /// with `ColocatedScale`. This prevents a caller from selecting a
    /// co-locating rule under the non-co-locating reference strategy or vice
    /// versa.
    pub fn validate(self, strategy: PageLayoutStrategy) -> PageLayoutResult<()> {
        let compatible = match self {
            Self::Separated { .. } => matches!(strategy, PageLayoutStrategy::VectorFirst),
            Self::FullColocation { .. } => matches!(strategy, PageLayoutStrategy::GraphFirst),
            Self::PartialColocation { .. } => {
                matches!(strategy, PageLayoutStrategy::ColocatedScale)
            }
        };
        if !compatible {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::ColocationStrategyMismatch,
            ));
        }
        strategy.validate_redundancy_factor(self.redundancy_factor())?;
        Ok(())
    }
}
