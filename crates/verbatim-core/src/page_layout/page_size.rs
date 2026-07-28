//! Validated SSD page sizes and alignment for NVMe co-located layouts.
//!
//! Page sizes are constrained to power-of-two values no smaller than the NVMe
//! logical block size and no larger than a validated ceiling, and the
//! alignment must evenly divide the page size. These are the physical-design
//! inputs the issue lists as empirical questions (4 KiB, 16 KiB, 64 KiB, or
//! measured alternatives); this module validates them without performing I/O.

use super::{PageLayoutDiagnosticCode, PageLayoutError, PageLayoutResult};

/// Minimum supported page size: the NVMe logical-block size floor.
pub const MIN_PAGE_SIZE_BYTES: u32 = 4_096;

/// Maximum supported page size before the layout is treated as unvalidated.
pub const MAX_PAGE_SIZE_BYTES: u32 = 1 << 20; // 1 MiB

/// A validated power-of-two SSD page size between the NVMe floor and ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageSize(u32);

impl PageSize {
    /// Constructs one of the canonical NVMe page sizes or fails closed.
    pub const fn kib_4() -> Self {
        Self(4_096)
    }

    /// Constructs the 16 KiB canonical page size.
    pub const fn kib_16() -> Self {
        Self(16_384)
    }

    /// Constructs the 64 kiB canonical page size.
    pub const fn kib_64() -> Self {
        Self(65_536)
    }

    /// Constructs a custom page size after validating power-of-two alignment
    /// and the supported range.
    pub fn new(bytes: u32) -> PageLayoutResult<Self> {
        if bytes == 0 {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::InvalidPageSize,
            ));
        }
        if bytes < MIN_PAGE_SIZE_BYTES {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::PageSizeTooSmall,
            ));
        }
        if bytes > MAX_PAGE_SIZE_BYTES {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::PageSizeTooLarge,
            ));
        }
        if !is_power_of_two(bytes) {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::PageSizeNotAligned,
            ));
        }
        Ok(Self(bytes))
    }

    /// Returns the validated page size in bytes.
    pub const fn bytes(self) -> u32 {
        self.0
    }
}

/// A validated power-of-two byte alignment that evenly divides a page size.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct PageAlignment(u32);

impl PageAlignment {
    /// Constructs the natural alignment equal to the page size.
    pub const fn natural(page: PageSize) -> Self {
        Self(page.bytes())
    }

    /// Constructs a custom alignment after validating power-of-two shape and
    /// that it evenly divides the supplied page size.
    pub fn new(bytes: u32, page: PageSize) -> PageLayoutResult<Self> {
        if bytes == 0 {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::InvalidAlignment,
            ));
        }
        if !is_power_of_two(bytes) {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::InvalidAlignment,
            ));
        }
        if !page.bytes().is_multiple_of(bytes) {
            return Err(PageLayoutError::contract(
                PageLayoutDiagnosticCode::AlignmentNotPageDivisor,
            ));
        }
        Ok(Self(bytes))
    }

    /// Returns the validated alignment in bytes.
    pub const fn bytes(self) -> u32 {
        self.0
    }
}

/// Returns `true` only for exact powers of two greater than zero.
const fn is_power_of_two(n: u32) -> bool {
    n != 0 && (n & (n - 1)) == 0
}
