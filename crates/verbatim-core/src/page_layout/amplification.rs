//! Bounded reads per query and the typed partial state on exhaustion.
//!
//! Read amplification is bounded two ways: by a maximum number of SSD pages
//! read per query and by a maximum number of bytes read per query. The byte
//! budget must be at least one page so a single vertex expansion is always
//! possible. Both bounds bind to `SearchBudget` page/byte caps so a plan can
//! never widen the caller's hard limits.

use super::page_size::PageSize;
use super::{PageLayoutDiagnosticCode, PageLayoutError, PageLayoutResult};

/// A typed partial state returned when a read-amplification bound is exhausted.
///
/// This is a contract marker only — it carries no partial results, neighbor
/// IDs, or vector data. A future provider maps it onto the typed partial
/// search state required by the `SearchBudget` exhaustion rule.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ReadAmplificationExhaustion {
    /// The maximum-pages-per-query bound was reached.
    MaxPages,
    /// The maximum-bytes-per-query bound was reached.
    MaxBytes,
}

/// Validated per-query read-amplification bounds.
///
/// `max_pages` and `max_bytes` are both positive, and `max_bytes` is at least
/// one [`PageSize`] so a vertex expansion is never impossible by construction.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReadAmplificationBound {
    max_pages: u64,
    max_bytes: u64,
}

impl ReadAmplificationBound {
    /// Constructs bounds after validating both caps are positive and the byte
    /// budget admits at least one full page read.
    pub fn new(max_pages: u64, max_bytes: u64, page: PageSize) -> PageLayoutResult<Self> {
        if max_pages == 0 {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::InvalidMaxPages,
            ));
        }
        if max_bytes == 0 {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::InvalidMaxBytes,
            ));
        }
        if max_bytes < u64::from(page.bytes()) {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::BytesBelowOnePage,
            ));
        }
        Ok(Self {
            max_pages,
            max_bytes,
        })
    }

    /// Returns the maximum pages readable per query.
    pub const fn max_pages(self) -> u64 {
        self.max_pages
    }

    /// Returns the maximum bytes readable per query.
    pub const fn max_bytes(self) -> u64 {
        self.max_bytes
    }

    /// Returns `true` when `consumed_pages`/`consumed_bytes` stay within bounds.
    pub const fn admits(self, consumed_pages: u64, consumed_bytes: u64) -> bool {
        consumed_pages <= self.max_pages && consumed_bytes <= self.max_bytes
    }

    /// Returns the typed exhaustion reason when the bounds are exceeded, else
    /// `None`.
    pub fn exhaustion(
        self,
        consumed_pages: u64,
        consumed_bytes: u64,
    ) -> Option<ReadAmplificationExhaustion> {
        if consumed_pages > self.max_pages {
            Some(ReadAmplificationExhaustion::MaxPages)
        } else if consumed_bytes > self.max_bytes {
            Some(ReadAmplificationExhaustion::MaxBytes)
        } else {
            None
        }
    }

    /// Rejects derived bounds that widen caller-provided hard caps.
    pub fn ensure_not_wider_than(self, caller: Self) -> PageLayoutResult<()> {
        if self.max_pages <= caller.max_pages && self.max_bytes <= caller.max_bytes {
            Ok(())
        } else {
            Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::BudgetExceeded,
            ))
        }
    }
}
