//! The aggregating page-layout specification and its `SearchBudget` binding.
//!
//! A [`PageLayoutSpec`] bundles a strategy, validated page size and alignment,
//! checksum policy, co-location rule, and read-amplification bound into one
//! fail-closed value. Construction validates every cross-field invariant:
//! strategy/rule compatibility, checksum policy consistency, the quality rule
//! that full-precision originals are separated from candidate codes, and that
//! derived page/byte bounds do not widen the caller's `SearchBudget`.

use crate::search_planner::SearchBudget;

use super::amplification::ReadAmplificationBound;
use super::checksum::ChecksumPolicy;
use super::colocation::ColocationRule;
use super::page_size::{PageAlignment, PageSize};
use super::strategy::PageLayoutStrategy;
use super::{PageLayoutDiagnosticCode, PageLayoutError, PageLayoutResult};

/// Field bag used to construct and validate a [`PageLayoutSpec`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageLayoutSpecFields {
    /// The selected co-located SSD page-layout strategy.
    pub strategy: PageLayoutStrategy,
    /// The validated page size.
    pub page_size: PageSize,
    /// The validated byte alignment.
    pub alignment: PageAlignment,
    /// The torn-page-detection checksum policy.
    pub checksum_policy: ChecksumPolicy,
    /// Which data co-locates beside a graph vertex, and the redundancy tradeoff.
    pub colocation: ColocationRule,
    /// Bounded reads per query.
    pub read_amplification: ReadAmplificationBound,
}

/// A fully validated AISAQ-style co-located SSD page-layout specification.
///
/// All cross-field invariants hold by construction. Co-located compressed
/// representations are candidate-generation aids only; full-precision originals
/// are preserved separately (enforced by the quality rule documented on each
/// co-location variant and re-checked here).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PageLayoutSpec {
    fields: PageLayoutSpecFields,
}

impl PageLayoutSpec {
    /// Constructs a spec after validating every cross-field invariant.
    pub fn new(fields: PageLayoutSpecFields) -> PageLayoutResult<Self> {
        // The page size and alignment are already validated newtypes; confirm
        // the alignment still divides this page size (it must, by construction
        // of PageAlignment, but the cross-check is cheap and explicit).
        if !fields
            .page_size
            .bytes()
            .is_multiple_of(fields.alignment.bytes())
        {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::AlignmentNotPageDivisor,
            ));
        }

        // Strategy/rule compatibility and the redundancy ceiling.
        fields.colocation.validate(fields.strategy)?;

        // A co-locating rule must run under checksums so a torn page cannot
        // silently corrupt duplicated candidate codes. The separated reference
        // rule may run without checksums.
        if fields.colocation.is_colocated() && !fields.checksum_policy.is_enabled() {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::ChecksumPolicyMismatch,
            ));
        }

        Ok(Self { fields })
    }

    /// Returns the validated field bag for inspection.
    pub const fn fields(&self) -> PageLayoutSpecFields {
        self.fields
    }

    /// Returns the selected strategy.
    pub const fn strategy(&self) -> PageLayoutStrategy {
        self.fields.strategy
    }

    /// Returns the validated page size.
    pub const fn page_size(&self) -> PageSize {
        self.fields.page_size
    }

    /// Returns the validated alignment.
    pub const fn alignment(&self) -> PageAlignment {
        self.fields.alignment
    }

    /// Returns the checksum policy.
    pub const fn checksum_policy(&self) -> ChecksumPolicy {
        self.fields.checksum_policy
    }

    /// Returns the co-location rule.
    pub const fn colocation(&self) -> ColocationRule {
        self.fields.colocation
    }

    /// Returns the read-amplification bound.
    pub const fn read_amplification(&self) -> ReadAmplificationBound {
        self.fields.read_amplification
    }

    /// Binds this spec's page/byte bounds against a caller-provided
    /// [`SearchBudget`] and rejects any widening.
    ///
    /// The spec's `max_pages` maps to `SearchBudget::max_ssd_pages` and its
    /// `max_bytes` maps to `SearchBudget::max_bytes_read`. A future provider
    /// returns the typed partial search state when this binding is exhausted.
    pub fn bind_to_budget(&self, budget: &SearchBudget) -> PageLayoutResult<()> {
        let caps = budget.fields();
        let amp = self.fields.read_amplification;
        if amp.max_pages() == 0 || amp.max_bytes() == 0 {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::BudgetBoundZero,
            ));
        }
        if amp.max_pages() > caps.max_ssd_pages || amp.max_bytes() > caps.max_bytes_read {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::BudgetExceeded,
            ));
        }
        Ok(())
    }
}
