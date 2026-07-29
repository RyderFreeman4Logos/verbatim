//! Bounded, privacy-neutral timing spans for retrieval pipeline stages.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::{TelemetryDiagnosticCode, TelemetryError, TelemetryResult};

/// Maximum duration a single retrieval stage may report: five minutes.
pub const MAX_STAGE_DURATION_MICROS: u64 = 300_000_000;

/// Closed retrieval pipeline stages that may receive a bounded span.
#[repr(u8)]
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanKind {
    RequestSetup,
    SelectivityEstimation,
    PlannerChoice,
    QueryEmbedding,
    DenseRetrieval,
    LexicalRetrieval,
    ExactRetrieval,
    GraphRetrieval,
    FilterCompilation,
    FusionDiversity,
    OriginalVectorRead,
    ExactRescoring,
    Reranking,
    EvidenceHydration,
    GraphExpansion,
    RemoteQueueNetwork,
    FallbackHandling,
}

impl SpanKind {
    /// Number of fixed stages in the contract.
    pub const COUNT: usize = 17;

    /// Every valid stage kind in stable telemetry order.
    pub const ALL: [Self; Self::COUNT] = [
        Self::RequestSetup,
        Self::SelectivityEstimation,
        Self::PlannerChoice,
        Self::QueryEmbedding,
        Self::DenseRetrieval,
        Self::LexicalRetrieval,
        Self::ExactRetrieval,
        Self::GraphRetrieval,
        Self::FilterCompilation,
        Self::FusionDiversity,
        Self::OriginalVectorRead,
        Self::ExactRescoring,
        Self::Reranking,
        Self::EvidenceHydration,
        Self::GraphExpansion,
        Self::RemoteQueueNetwork,
        Self::FallbackHandling,
    ];

    /// Stable, low-cardinality stage name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RequestSetup => "request_setup",
            Self::SelectivityEstimation => "selectivity_estimation",
            Self::PlannerChoice => "planner_choice",
            Self::QueryEmbedding => "query_embedding",
            Self::DenseRetrieval => "dense_retrieval",
            Self::LexicalRetrieval => "lexical_retrieval",
            Self::ExactRetrieval => "exact_retrieval",
            Self::GraphRetrieval => "graph_retrieval",
            Self::FilterCompilation => "filter_compilation",
            Self::FusionDiversity => "fusion_diversity",
            Self::OriginalVectorRead => "original_vector_read",
            Self::ExactRescoring => "exact_rescoring",
            Self::Reranking => "reranking",
            Self::EvidenceHydration => "evidence_hydration",
            Self::GraphExpansion => "graph_expansion",
            Self::RemoteQueueNetwork => "remote_queue_network",
            Self::FallbackHandling => "fallback_handling",
        }
    }

    pub(crate) const fn as_index(self) -> usize {
        self as usize
    }
}

impl fmt::Display for SpanKind {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(self.as_str())
    }
}

/// Serializable construction fields for [`StageSpan`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct StageSpanFields {
    pub kind: SpanKind,
    pub start_micros: u64,
    pub end_micros: u64,
}

/// A completed stage timing record with a fixed duration ceiling.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct StageSpan {
    kind: SpanKind,
    start_micros: u64,
    end_micros: u64,
}

impl<'de> Deserialize<'de> for StageSpan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = StageSpanFields::deserialize(deserializer)?;
        Self::new(fields.kind, fields.start_micros, fields.end_micros)
            .map_err(serde::de::Error::custom)
    }
}

impl StageSpan {
    /// Creates a complete stage span after validating timing order and duration.
    pub fn new(kind: SpanKind, start_micros: u64, end_micros: u64) -> TelemetryResult<Self> {
        if end_micros < start_micros {
            return Err(TelemetryError::contract(
                TelemetryDiagnosticCode::InvalidSpanTiming,
            ));
        }
        let duration_micros =
            end_micros
                .checked_sub(start_micros)
                .ok_or(TelemetryError::contract(
                    TelemetryDiagnosticCode::InvalidSpanTiming,
                ))?;
        if duration_micros > MAX_STAGE_DURATION_MICROS {
            return Err(TelemetryError::contract(
                TelemetryDiagnosticCode::SpanDurationExceeded,
            ));
        }
        Ok(Self {
            kind,
            start_micros,
            end_micros,
        })
    }

    /// Adds bounded elapsed time without allowing integer or duration overflow.
    pub fn extend_by_micros(&mut self, additional_micros: u64) -> TelemetryResult<()> {
        let end_micros =
            self.end_micros
                .checked_add(additional_micros)
                .ok_or(TelemetryError::contract(
                    TelemetryDiagnosticCode::SpanDurationExceeded,
                ))?;
        let next = Self::new(self.kind, self.start_micros, end_micros)?;
        *self = next;
        Ok(())
    }

    /// Returns the stage's closed kind.
    pub const fn kind(self) -> SpanKind {
        self.kind
    }

    /// Returns the monotonic start timestamp in microseconds.
    pub const fn start_micros(self) -> u64 {
        self.start_micros
    }

    /// Returns the monotonic end timestamp in microseconds.
    pub const fn end_micros(self) -> u64 {
        self.end_micros
    }

    /// Returns the validated elapsed duration in microseconds.
    pub const fn duration_micros(self) -> u64 {
        self.end_micros - self.start_micros
    }

    /// Revalidates timing order and the hard duration bound.
    pub fn validate(&self) -> TelemetryResult<()> {
        Self::new(self.kind, self.start_micros, self.end_micros).map(|_| ())
    }

    /// Returns serializable fields that remain subject to deserialization checks.
    pub const fn as_fields(self) -> StageSpanFields {
        StageSpanFields {
            kind: self.kind,
            start_micros: self.start_micros,
            end_micros: self.end_micros,
        }
    }
}
