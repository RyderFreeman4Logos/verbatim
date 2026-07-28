//! Authorized cardinality estimates with redacted diagnostics.

use std::fmt;

use super::{SearchPlannerDiagnosticCode, SearchPlannerError, SearchPlannerResult};

/// Confidence assigned by the authorized statistics source.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CardinalityConfidence {
    /// The source has recently verified the estimate.
    High,
    /// The source has a bounded but less certain estimate.
    Medium,
    /// The source cannot support an ordinary-path decision safely.
    Low,
}

/// Freshness of the authorized statistics snapshot.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CardinalityFreshness {
    /// The snapshot is current for its bound generation.
    Fresh,
    /// The snapshot may no longer describe the bound generation.
    Stale,
}

/// Authorized source that produced a cardinality estimate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum CardinalitySource {
    /// Tenant- and ACL-bound catalog statistics.
    AuthorizedCatalog,
    /// Tenant- and ACL-bound bitmap cardinality.
    AuthorizedBitmap,
    /// Tenant- and ACL-bound scalar-predicate statistics.
    AuthorizedScalarStatistics,
    /// A backend telemetry snapshot whose authorization boundary is known.
    BackendTelemetry,
}

/// Whether an estimate is known to be trustworthy for path selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EstimateReliability {
    /// The estimate has no known correctness defect.
    Verified,
    /// A validation process has marked the estimate as wrong.
    KnownWrong,
}

/// Required handling for stale, low-confidence, or known-wrong estimates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum EstimateHandlingMode {
    /// Reject the request rather than plan against an untrusted estimate.
    FailClosed,
    /// Return only a named, still hard-bounded degraded profile.
    NamedBoundedDegradedProfile,
}

/// ACL-bound cardinality information used internally by the planner.
///
/// The raw count is intentionally private and is redacted from `Debug`; callers
/// receive a plan or closed diagnostic rather than a tenant corpus-size value.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct CardinalityEstimate {
    matching_count: u64,
    confidence: CardinalityConfidence,
    freshness: CardinalityFreshness,
    source: CardinalitySource,
    reliability: EstimateReliability,
}

impl CardinalityEstimate {
    /// Creates an estimate supplied by an authorization-bound statistics adapter.
    pub const fn new(
        matching_count: u64,
        confidence: CardinalityConfidence,
        freshness: CardinalityFreshness,
        source: CardinalitySource,
        reliability: EstimateReliability,
    ) -> Self {
        Self {
            matching_count,
            confidence,
            freshness,
            source,
            reliability,
        }
    }

    /// Returns the source that produced the estimate without exposing its count.
    pub const fn source(&self) -> CardinalitySource {
        self.source
    }

    /// Returns the confidence class without exposing its count.
    pub const fn confidence(&self) -> CardinalityConfidence {
        self.confidence
    }

    /// Returns the freshness class without exposing its count.
    pub const fn freshness(&self) -> CardinalityFreshness {
        self.freshness
    }

    pub(crate) const fn matching_count(&self) -> u64 {
        self.matching_count
    }

    pub(crate) fn disposition(
        &self,
        mode: EstimateHandlingMode,
    ) -> SearchPlannerResult<EstimateDisposition> {
        let stale = self.freshness == CardinalityFreshness::Stale;
        let untrusted = self.reliability == EstimateReliability::KnownWrong
            || self.confidence == CardinalityConfidence::Low;
        if !stale && !untrusted {
            return Ok(EstimateDisposition::Trusted);
        }
        match mode {
            EstimateHandlingMode::NamedBoundedDegradedProfile => Ok(EstimateDisposition::Degraded),
            EstimateHandlingMode::FailClosed if stale => Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::StaleCardinalityEstimate,
            )),
            EstimateHandlingMode::FailClosed => Err(SearchPlannerError::new(
                SearchPlannerDiagnosticCode::UntrustedCardinalityEstimate,
            )),
        }
    }
}

impl fmt::Debug for CardinalityEstimate {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("CardinalityEstimate")
            .field("matching_count", &"<redacted>")
            .field("confidence", &self.confidence)
            .field("freshness", &self.freshness)
            .field("source", &self.source)
            .field("reliability", &self.reliability)
            .finish()
    }
}

/// Internal result of applying estimate-trust policy.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum EstimateDisposition {
    /// The estimate may select an ordinary retrieval path.
    Trusted,
    /// Only a named, bounded degraded profile may be returned.
    Degraded,
}
