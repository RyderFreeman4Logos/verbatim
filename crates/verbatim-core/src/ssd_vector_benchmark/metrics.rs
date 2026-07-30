//! Quality and resource metric schemas for EVAL-SSD-001.
//!
//! Refs #382.

use serde::{Deserialize, Serialize};

use super::error::{
    SsdVectorBenchmarkDiagnosticCode, SsdVectorBenchmarkError, SsdVectorBenchmarkResult,
};

/// Whether a quality metric is for candidate generation or final rescore ranking.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityStage {
    /// ANN / retriever candidate generation quality (before original-vector rescore).
    Candidate,
    /// Final ranking quality after original full-precision rescore.
    FinalRescore,
}

impl QualityStage {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Candidate => "candidate",
            Self::FinalRescore => "final_rescore",
        }
    }
}

/// Closed quality metric kinds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QualityMetricKind {
    RecallAt10,
    RecallAt20,
    RecallAt50,
    RecallAt100,
    NdcgAt10,
    Mrr,
    FilteredAuthorizedSubsetRecall,
    RankCorrelation,
    TopKOverlap,
    ExactLocatorAccuracy,
    UpdateStreamRecallDrift,
}

impl QualityMetricKind {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::RecallAt10 => "recall_at_10",
            Self::RecallAt20 => "recall_at_20",
            Self::RecallAt50 => "recall_at_50",
            Self::RecallAt100 => "recall_at_100",
            Self::NdcgAt10 => "ndcg_at_10",
            Self::Mrr => "mrr",
            Self::FilteredAuthorizedSubsetRecall => "filtered_authorized_subset_recall",
            Self::RankCorrelation => "rank_correlation",
            Self::TopKOverlap => "top_k_overlap",
            Self::ExactLocatorAccuracy => "exact_locator_accuracy",
            Self::UpdateStreamRecallDrift => "update_stream_recall_drift",
        }
    }
}

/// One observed quality metric value, staged as candidate or final.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct QualityMetricObservation {
    stage: QualityStage,
    kind: QualityMetricKind,
    value: f64,
}

/// Construction fields for [`QualityMetricObservation`].
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct QualityMetricObservationFields {
    pub stage: QualityStage,
    pub kind: QualityMetricKind,
    pub value: f64,
}

impl QualityMetricObservation {
    /// Builds a quality observation. Value must be finite and in [0.0, 1.0]
    /// except update-stream drift which may be in [0.0, 1.0] absolute drift.
    pub fn new(fields: QualityMetricObservationFields) -> SsdVectorBenchmarkResult<Self> {
        if !fields.value.is_finite() || !(0.0..=1.0).contains(&fields.value) {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::InvalidBounds,
            ));
        }
        Ok(Self {
            stage: fields.stage,
            kind: fields.kind,
            value: fields.value,
        })
    }

    pub const fn stage(self) -> QualityStage {
        self.stage
    }

    pub const fn kind(self) -> QualityMetricKind {
        self.kind
    }

    pub const fn value(self) -> f64 {
        self.value
    }
}

impl<'de> Deserialize<'de> for QualityMetricObservation {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = QualityMetricObservationFields::deserialize(deserializer)?;
        Self::new(fields).map_err(serde::de::Error::custom)
    }
}

/// Bundle of candidate and final quality observations for one scenario.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct QualityMetrics {
    observations: Vec<QualityMetricObservation>,
}

/// Construction fields for [`QualityMetrics`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QualityMetricsFields {
    pub observations: Vec<QualityMetricObservationFields>,
}

impl QualityMetrics {
    /// Builds quality metrics. Candidate and final stages must both appear.
    pub fn new(observations: Vec<QualityMetricObservation>) -> SsdVectorBenchmarkResult<Self> {
        if observations.is_empty() {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::MissingComponent,
            ));
        }
        let has_candidate = observations
            .iter()
            .any(|o| o.stage() == QualityStage::Candidate);
        let has_final = observations
            .iter()
            .any(|o| o.stage() == QualityStage::FinalRescore);
        if !has_candidate || !has_final {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::CandidateFinalQualityMustBeSeparate,
            ));
        }
        Ok(Self { observations })
    }

    pub fn observations(&self) -> &[QualityMetricObservation] {
        &self.observations
    }

    /// Lookup a single observation by stage and kind.
    pub fn get(&self, stage: QualityStage, kind: QualityMetricKind) -> Option<f64> {
        self.observations
            .iter()
            .find(|o| o.stage() == stage && o.kind() == kind)
            .map(|o| o.value())
    }
}

impl<'de> Deserialize<'de> for QualityMetrics {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = QualityMetricsFields::deserialize(deserializer)?;
        let mut observations = Vec::with_capacity(fields.observations.len());
        for entry in fields.observations {
            observations
                .push(QualityMetricObservation::new(entry).map_err(serde::de::Error::custom)?);
        }
        Self::new(observations).map_err(serde::de::Error::custom)
    }
}

/// Cgroup v2 memory accounting required in every resource measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub struct CgroupMemoryMeasurement {
    memory_current_bytes: u64,
    memory_high_bytes: u64,
    memory_max_bytes: u64,
    peak_bytes: u64,
    measured: bool,
}

/// Construction fields for [`CgroupMemoryMeasurement`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct CgroupMemoryMeasurementFields {
    pub memory_current_bytes: u64,
    pub memory_high_bytes: u64,
    pub memory_max_bytes: u64,
    pub peak_bytes: u64,
    /// When false, gate evaluation fails closed as missing measurement.
    pub measured: bool,
}

impl CgroupMemoryMeasurement {
    /// Builds a cgroup memory measurement. `measured` must be true for gate pass.
    pub fn new(fields: CgroupMemoryMeasurementFields) -> SsdVectorBenchmarkResult<Self> {
        if fields.memory_high_bytes == 0 || fields.memory_max_bytes == 0 {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::InvalidBounds,
            ));
        }
        if fields.memory_high_bytes > fields.memory_max_bytes {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::InvalidBounds,
            ));
        }
        if fields.peak_bytes < fields.memory_current_bytes {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::InvalidBounds,
            ));
        }
        Ok(Self {
            memory_current_bytes: fields.memory_current_bytes,
            memory_high_bytes: fields.memory_high_bytes,
            memory_max_bytes: fields.memory_max_bytes,
            peak_bytes: fields.peak_bytes,
            measured: fields.measured,
        })
    }

    /// Explicit unknown/missing measurement (fails gates).
    pub fn unknown() -> Self {
        Self {
            memory_current_bytes: 0,
            memory_high_bytes: 1,
            memory_max_bytes: 1,
            peak_bytes: 0,
            measured: false,
        }
    }

    pub const fn memory_current_bytes(self) -> u64 {
        self.memory_current_bytes
    }

    pub const fn memory_high_bytes(self) -> u64 {
        self.memory_high_bytes
    }

    pub const fn memory_max_bytes(self) -> u64 {
        self.memory_max_bytes
    }

    pub const fn peak_bytes(self) -> u64 {
        self.peak_bytes
    }

    pub const fn measured(self) -> bool {
        self.measured
    }

    /// Fail-closed check used by gate evaluation.
    pub fn require_measured(self) -> SsdVectorBenchmarkResult<()> {
        if !self.measured {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::MissingCgroupMemoryMeasurement,
            ));
        }
        Ok(())
    }
}

impl<'de> Deserialize<'de> for CgroupMemoryMeasurement {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = CgroupMemoryMeasurementFields::deserialize(deserializer)?;
        Self::new(fields).map_err(serde::de::Error::custom)
    }
}

/// Latency percentiles in microseconds.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct LatencyMicros {
    pub p50: u64,
    pub p95: u64,
    pub p99: u64,
}

impl LatencyMicros {
    /// Builds latency percentiles with ordering p50 <= p95 <= p99.
    pub fn new(p50: u64, p95: u64, p99: u64) -> SsdVectorBenchmarkResult<Self> {
        if p50 > p95 || p95 > p99 {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::InvalidBounds,
            ));
        }
        Ok(Self { p50, p95, p99 })
    }
}

/// Resource / performance metrics for one backend scenario.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResourceMetrics {
    latency: LatencyMicros,
    throughput_qps_milli: u64,
    cgroup_memory: CgroupMemoryMeasurement,
    major_faults: Option<u64>,
    minor_faults: Option<u64>,
    ssd_bytes_per_query: Option<u64>,
    ssd_ops_per_query: Option<u64>,
    index_bytes: Option<u64>,
}

/// Construction fields for [`ResourceMetrics`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ResourceMetricsFields {
    pub latency: LatencyMicros,
    pub throughput_qps_milli: u64,
    pub cgroup_memory: CgroupMemoryMeasurementFields,
    pub major_faults: Option<u64>,
    pub minor_faults: Option<u64>,
    pub ssd_bytes_per_query: Option<u64>,
    pub ssd_ops_per_query: Option<u64>,
    pub index_bytes: Option<u64>,
}

impl ResourceMetrics {
    /// Builds resource metrics. Cgroup measurement is always present; unknown fails gates.
    pub fn new(fields: ResourceMetricsFields) -> SsdVectorBenchmarkResult<Self> {
        // Re-validate latency ordering even if constructed via struct literal in tests.
        let latency =
            LatencyMicros::new(fields.latency.p50, fields.latency.p95, fields.latency.p99)?;
        Ok(Self {
            latency,
            throughput_qps_milli: fields.throughput_qps_milli,
            cgroup_memory: CgroupMemoryMeasurement::new(fields.cgroup_memory)?,
            major_faults: fields.major_faults,
            minor_faults: fields.minor_faults,
            ssd_bytes_per_query: fields.ssd_bytes_per_query,
            ssd_ops_per_query: fields.ssd_ops_per_query,
            index_bytes: fields.index_bytes,
        })
    }

    pub const fn latency(&self) -> LatencyMicros {
        self.latency
    }

    pub const fn throughput_qps_milli(&self) -> u64 {
        self.throughput_qps_milli
    }

    pub const fn cgroup_memory(&self) -> CgroupMemoryMeasurement {
        self.cgroup_memory
    }

    pub const fn major_faults(&self) -> Option<u64> {
        self.major_faults
    }

    pub const fn minor_faults(&self) -> Option<u64> {
        self.minor_faults
    }

    pub const fn ssd_bytes_per_query(&self) -> Option<u64> {
        self.ssd_bytes_per_query
    }

    pub const fn ssd_ops_per_query(&self) -> Option<u64> {
        self.ssd_ops_per_query
    }

    pub const fn index_bytes(&self) -> Option<u64> {
        self.index_bytes
    }
}

impl<'de> Deserialize<'de> for ResourceMetrics {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = ResourceMetricsFields::deserialize(deserializer)?;
        Self::new(fields).map_err(serde::de::Error::custom)
    }
}
