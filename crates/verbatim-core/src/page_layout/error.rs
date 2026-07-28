//! Fail-closed, diagnostic-code-only errors for the AISAQ page-layout contract.
//!
//! No payload, vector data, neighbor IDs, offsets, or checksum bytes are ever
//! retained on an error. [`PageLayoutError`] renders only a stable diagnostic
//! code string, so partial page contents cannot leak through `Debug`/`Display`.

use std::error::Error;
use std::fmt;

/// Result alias for page-layout contract operations.
pub type PageLayoutResult<T> = Result<T, PageLayoutError>;

/// Closed diagnostic taxonomy for AISAQ co-located SSD page-layout validation.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PageLayoutDiagnosticCode {
    /// Page size was zero or not a power of two.
    InvalidPageSize,
    /// Page size was below the minimum supported NVMe page size.
    PageSizeTooSmall,
    /// Page size exceeded the validated ceiling.
    PageSizeTooLarge,
    /// Custom page size was not power-of-two aligned.
    PageSizeNotAligned,
    /// Alignment was zero or not a power of two.
    InvalidAlignment,
    /// Alignment did not divide the page size evenly.
    AlignmentNotPageDivisor,
    /// Redundancy factor was zero.
    InvalidRedundancyFactor,
    /// Redundancy factor exceeded the documented constant-factor ceiling.
    RedundancyFactorTooLarge,
    /// Maximum pages bound was zero.
    InvalidMaxPages,
    /// Maximum bytes bound was zero.
    InvalidMaxBytes,
    /// Byte budget was smaller than a single page read.
    BytesBelowOnePage,
    /// Checksum payload was empty.
    EmptyChecksumPayload,
    /// Stored and recomputed checksums disagreed (torn or corrupted page).
    ChecksumMismatch,
    /// Spec mixed an incompatible strategy with a co-location rule.
    ColocationStrategyMismatch,
    /// Spec mixed an incompatible checksum policy with a co-location rule.
    ChecksumPolicyMismatch,
    /// Full-precision originals were not separated from candidate codes.
    FullPrecisionNotSeparated,
    /// A derived budget exceeded the caller-provided `SearchBudget`.
    BudgetExceeded,
    /// A budget-derived bound was zero.
    BudgetBoundZero,
    /// Candidate-code representation was requested as a replacement for originals.
    CandidateCodeNotCandidateAid,
}

impl PageLayoutDiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidPageSize => "invalid_page_size",
            Self::PageSizeTooSmall => "page_size_too_small",
            Self::PageSizeTooLarge => "page_size_too_large",
            Self::PageSizeNotAligned => "page_size_not_aligned",
            Self::InvalidAlignment => "invalid_alignment",
            Self::AlignmentNotPageDivisor => "alignment_not_page_divisor",
            Self::InvalidRedundancyFactor => "invalid_redundancy_factor",
            Self::RedundancyFactorTooLarge => "redundancy_factor_too_large",
            Self::InvalidMaxPages => "invalid_max_pages",
            Self::InvalidMaxBytes => "invalid_max_bytes",
            Self::BytesBelowOnePage => "bytes_below_one_page",
            Self::EmptyChecksumPayload => "empty_checksum_payload",
            Self::ChecksumMismatch => "checksum_mismatch",
            Self::ColocationStrategyMismatch => "colocation_strategy_mismatch",
            Self::ChecksumPolicyMismatch => "checksum_policy_mismatch",
            Self::FullPrecisionNotSeparated => "full_precision_not_separated",
            Self::BudgetExceeded => "budget_exceeded",
            Self::BudgetBoundZero => "budget_bound_zero",
            Self::CandidateCodeNotCandidateAid => "candidate_code_not_candidate_aid",
        }
    }
}

/// A page-layout contract failure that retains only a stable diagnostic code.
///
/// The enum is `Copy` and carries no payload, so a partial SSD page, neighbor
/// list, offset, or checksum digest can never escape through this error type.
#[derive(Clone, Copy, PartialEq, Eq)]
pub enum PageLayoutError {
    Contract { code: PageLayoutDiagnosticCode },
}

impl PageLayoutError {
    pub const fn contract(code: PageLayoutDiagnosticCode) -> Self {
        Self::Contract { code }
    }

    pub const fn diagnostic_code(self) -> PageLayoutDiagnosticCode {
        match self {
            Self::Contract { code } => code,
        }
    }
}

impl fmt::Debug for PageLayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "PageLayoutError({})",
            self.diagnostic_code().as_str()
        )
    }
}

impl fmt::Display for PageLayoutError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "page-layout.{}", self.diagnostic_code().as_str())
    }
}

impl Error for PageLayoutError {}
