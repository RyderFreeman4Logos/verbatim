//! Full-dimensional SSD vector benchmark contract (issue #382 / EVAL-SSD-001).
//!
//! This pure walking-skeleton defines closed systems-under-test, corpus and
//! query-matrix identities, quality/resource metric schemas, hard acceptance
//! gates, deterministic local-subset plan/run, and machine-readable plus
//! Markdown report emission. It has no live Qdrant/LanceDB/Milvus clients,
//! no 1M/10M corpus generation, and no cgroup orchestration.
//!
//! See `docs/architecture/ssd-vector-benchmark.md`.
//!
//! Refs #382.

mod corpus;
mod error;
mod gate;
mod identity;
mod metrics;
mod query_matrix;
mod report;
mod run;
mod system;

pub use corpus::{
    CorpusIdentity, CorpusIdentityFields, CorpusScale, GroundTruthConfig, GroundTruthConfigFields,
    OriginalVectorPrecision, StorageGrowthPoint, StorageGrowthSeries, StorageGrowthSeriesFields,
};
pub use error::{
    SsdVectorBenchmarkDiagnosticCode, SsdVectorBenchmarkError, SsdVectorBenchmarkResult,
};
pub use gate::{
    evaluate_quality_gates, evaluate_resource_gates, evaluate_scenario_gates,
    evaluate_suite_verdict, require_pass, BackendGateOutcome, GateVerdict, HardGatePolicy,
    LatencyGateThresholds, MemoryGateThresholds, QualityGateThresholds, ScenarioGateInput,
};
pub use identity::{
    ClosedLabel, ComparisonIdentity, ComparisonIdentityFields, ContentDigest,
    MAX_CLOSED_LABEL_BYTES, MAX_DIGEST_HEX_BYTES,
};
pub use metrics::{
    CgroupMemoryMeasurement, CgroupMemoryMeasurementFields, LatencyMicros, QualityMetricKind,
    QualityMetricObservation, QualityMetricObservationFields, QualityMetrics, QualityMetricsFields,
    QualityStage, ResourceMetrics, ResourceMetricsFields,
};
pub use query_matrix::{
    CacheState, ConcurrencyLevel, FilterSelectivity, QueryClass, QueryMatrix, QueryMatrixFields,
    QueryScenario, QueryScenarioFields, UpdateState,
};
pub use report::{
    BackendScenarioResult, BackendScenarioResultFields, BenchmarkReport, BenchmarkReportFields,
    BenchmarkReportParts, HardwareProfile, HardwareProfileFields,
    SSD_VECTOR_BENCHMARK_REPORT_SCHEMA_VERSION,
};
pub use run::{
    all_passing_local_subset_measurements, passing_injected_measurement, report_schema_version,
    InjectedScenarioMeasurement, LocalSubsetPlan, SSD_VECTOR_BENCHMARK_PLAN_SCHEMA_VERSION,
};
pub use system::{
    BackendId, BackendRole, SystemUnderTest, SystemUnderTestFields, SystemsCatalog,
    SystemsCatalogFields, REQUIRED_VECTOR_DIMENSION,
};

/// Contract schema version for the SSD vector benchmark module surface.
pub const SSD_VECTOR_BENCHMARK_CONTRACT_SCHEMA_VERSION: u32 = 1;
