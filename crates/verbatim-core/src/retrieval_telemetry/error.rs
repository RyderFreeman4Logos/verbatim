//! Closed, payload-free diagnostics for retrieval telemetry contracts.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Result alias for retrieval-telemetry contract operations.
pub type TelemetryResult<T> = Result<T, TelemetryError>;

/// Closed diagnostic taxonomy for retrieval-telemetry validation failures.
///
/// Variants intentionally retain no query text, evidence, paths, identifiers,
/// backend values, or other caller-controlled payloads.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TelemetryDiagnosticCode {
    /// A span ended before it started.
    InvalidSpanTiming,
    /// A stage duration exceeded the contract's fixed upper bound.
    SpanDurationExceeded,
    /// Adding a telemetry counter would overflow its fixed-width representation.
    CounterOverflow,
    /// A cgroup-memory snapshot violates its structural invariants.
    InvalidMemorySnapshot,
    /// A cgroup-memory snapshot exceeds a fixed representable bound.
    MemorySnapshotExceedsBound,
    /// A backend attribute key/value pairing is not part of the closed contract.
    InvalidBackendAttribute,
    /// A numeric backend attribute is zero when positive or exceeds its bound.
    BackendAttributeValueOutOfBounds,
    /// An opaque correlation token was empty, oversized, or malformed.
    InvalidRedactedTelemetryId,
    /// A field class is forbidden at the requested telemetry destination.
    PrivacyPolicyViolation,
}

impl TelemetryDiagnosticCode {
    /// Every closed code, for exhaustive contract tests and stable adapters.
    pub const ALL: [Self; 9] = [
        Self::InvalidSpanTiming,
        Self::SpanDurationExceeded,
        Self::CounterOverflow,
        Self::InvalidMemorySnapshot,
        Self::MemorySnapshotExceedsBound,
        Self::InvalidBackendAttribute,
        Self::BackendAttributeValueOutOfBounds,
        Self::InvalidRedactedTelemetryId,
        Self::PrivacyPolicyViolation,
    ];

    /// Returns the stable machine-readable code.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::InvalidSpanTiming => "invalid_span_timing",
            Self::SpanDurationExceeded => "span_duration_exceeded",
            Self::CounterOverflow => "counter_overflow",
            Self::InvalidMemorySnapshot => "invalid_memory_snapshot",
            Self::MemorySnapshotExceedsBound => "memory_snapshot_exceeds_bound",
            Self::InvalidBackendAttribute => "invalid_backend_attribute",
            Self::BackendAttributeValueOutOfBounds => "backend_attribute_value_out_of_bounds",
            Self::InvalidRedactedTelemetryId => "invalid_redacted_telemetry_id",
            Self::PrivacyPolicyViolation => "privacy_policy_violation",
        }
    }
}

/// A fail-closed telemetry failure containing only a stable diagnostic code.
#[derive(Clone, Copy, PartialEq, Eq)]
pub struct TelemetryError {
    code: TelemetryDiagnosticCode,
}

impl TelemetryError {
    /// Builds a diagnostic-only error without any caller-controlled data.
    pub const fn contract(code: TelemetryDiagnosticCode) -> Self {
        Self { code }
    }

    /// Returns the closed diagnostic code.
    pub const fn diagnostic_code(self) -> TelemetryDiagnosticCode {
        self.code
    }
}

impl fmt::Debug for TelemetryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "TelemetryError({})", self.code.as_str())
    }
}

impl fmt::Display for TelemetryError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "retrieval-telemetry.{}", self.code.as_str())
    }
}

impl Error for TelemetryError {}
