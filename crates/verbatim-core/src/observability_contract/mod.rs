//! Cross-service observability contract (OBS-001 / issue #338).
//!
//! Walking skeleton: typed correlation IDs, bounded spans with links, low-
//! cardinality metrics, structured logs with automatic redaction, cardinality
//! guards, and SLO/error-budget definitions. OpenTelemetry-compatible *shapes*
//! only — no OTLP exporter, no live daemon wiring.
//!
//! Residual: stage instrumentation, queue baggage trust, OTLP export, local
//! diagnostics adapters, closing #338. See
//! `docs/architecture/cross-service-observability.md`.

mod common;
mod log;
mod metric;
mod slo;
mod span;
mod trace;

pub use log::{
    decode_log_entry_json, decode_redaction_policy_json, LogEntry, LogEntryParams, LogLevel,
    RedactionPolicy, SensitiveKind,
};
pub use metric::{
    decode_metric_spec_json, CardinalityGuard, LabelPrivacy, MetricKind, MetricLabelSpec,
    MetricSpec, DEFAULT_MAX_LABEL_CARDINALITY,
};
pub use slo::{
    decode_slo_definition_json, ErrorBudgetStatus, LatencyTarget, SloDefinition,
    SloDefinitionParams, SloFailureDomain,
};
pub use span::{
    decode_span_spec_json, SpanLink, SpanOpenParams, SpanSpec, SpanStatus, MAX_SPAN_ATTRIBUTES,
    MAX_SPAN_LINKS,
};
pub use trace::{decode_trace_context_json, TraceContext, TraceContextFields};

/// Wire schema version for observability documents. Unknown versions fail closed.
pub const OBSERVABILITY_CONTRACT_SCHEMA_VERSION: u32 = 1;

#[cfg(test)]
#[path = "../observability_contract_tests.rs"]
mod tests;
