//! Deterministic local-subset plan and run contract.
//!
//! Refs #382. Live Qdrant/LanceDB/Milvus network clients are intentionally out
//! of scope. Harness inputs may inject recorded measurements for in-process
//! backends and reference stubs.

use serde::{Deserialize, Serialize};

use super::corpus::{
    CorpusIdentity, StorageGrowthPoint, StorageGrowthSeries, StorageGrowthSeriesFields,
};
use super::error::{
    SsdVectorBenchmarkDiagnosticCode, SsdVectorBenchmarkError, SsdVectorBenchmarkResult,
};
use super::gate::{
    evaluate_scenario_gates, evaluate_suite_verdict, BackendGateOutcome, GateVerdict,
    HardGatePolicy, ScenarioGateInput,
};
use super::identity::{ComparisonIdentity, ComparisonIdentityFields};
use super::metrics::{
    CgroupMemoryMeasurementFields, LatencyMicros, QualityMetricKind, QualityMetricObservation,
    QualityMetricObservationFields, QualityMetrics, QualityStage, ResourceMetrics,
    ResourceMetricsFields,
};
use super::query_matrix::{CacheState, QueryMatrix};
use super::report::{
    BackendScenarioResult, BenchmarkReport, HardwareProfile,
    SSD_VECTOR_BENCHMARK_REPORT_SCHEMA_VERSION,
};
use super::system::{BackendId, BackendRole, SystemsCatalog, REQUIRED_VECTOR_DIMENSION};

/// Contract schema version for local-subset plans.
pub const SSD_VECTOR_BENCHMARK_PLAN_SCHEMA_VERSION: u32 = 1;

/// Injected harness measurement for one backend × scenario cell.
///
/// Used by tests and future adapters; not a live network client.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct InjectedScenarioMeasurement {
    pub backend_id: BackendId,
    pub scenario_id: String,
    pub cache_state: CacheState,
    pub quality: super::metrics::QualityMetricsFields,
    pub resources: ResourceMetricsFields,
}

/// Deterministic local-subset plan inputs.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct LocalSubsetPlan {
    plan_id: super::identity::ClosedLabel,
    schema_version: u32,
    git_revision: super::identity::ClosedLabel,
    config_digest: super::identity::ContentDigest,
    comparison_identity: ComparisonIdentity,
    corpus: CorpusIdentity,
    systems: SystemsCatalog,
    query_matrix: QueryMatrix,
    hardware: HardwareProfile,
    gate_policy: HardGatePolicy,
    storage_growth: StorageGrowthSeries,
}

impl LocalSubsetPlan {
    /// Constructs the deterministic local-subset plan with program defaults.
    pub fn deterministic_default(
        git_revision: impl Into<String>,
        config_digest: impl Into<String>,
    ) -> SsdVectorBenchmarkResult<Self> {
        let comparison_identity = ComparisonIdentity::new(ComparisonIdentityFields {
            vectors_digest: "0123456789abcdef0123456789abcdef".to_string(),
            filters_digest: "fedcba9876543210fedcba9876543210".to_string(),
            budgets_digest: "aabbccddeeff00112233445566778899".to_string(),
            qrels_digest: "99aa88bb77cc66dd55ee44ff33221100".to_string(),
            final_scoring_policy: "exact-f32-cosine-full-dim-v1".to_string(),
            dimension: REQUIRED_VECTOR_DIMENSION,
        })?;
        let corpus = CorpusIdentity::local_subset_default()?;
        let systems = SystemsCatalog::local_subset_defaults()?;
        let query_matrix = QueryMatrix::local_subset_default()?;
        let hardware = HardwareProfile::local_subset_default()?;
        let storage_growth = StorageGrowthSeries::new(StorageGrowthSeriesFields {
            by_vector_count: vec![
                StorageGrowthPoint {
                    x: 512,
                    index_bytes: 4_096_000,
                },
                StorageGrowthPoint {
                    x: 1_024,
                    index_bytes: 8_192_000,
                },
            ],
            by_source_count: vec![
                StorageGrowthPoint {
                    x: 4,
                    index_bytes: 4_096_000,
                },
                StorageGrowthPoint {
                    x: 8,
                    index_bytes: 8_192_000,
                },
            ],
        })?;
        Ok(Self {
            plan_id: super::identity::ClosedLabel::new("local-subset-plan-v1")?,
            schema_version: SSD_VECTOR_BENCHMARK_PLAN_SCHEMA_VERSION,
            git_revision: super::identity::ClosedLabel::new(git_revision)?,
            config_digest: super::identity::ContentDigest::new(config_digest)?,
            comparison_identity,
            corpus,
            systems,
            query_matrix,
            hardware,
            gate_policy: HardGatePolicy::program_default(),
            storage_growth,
        })
    }

    pub fn plan_id(&self) -> &str {
        self.plan_id.as_str()
    }

    pub const fn schema_version(&self) -> u32 {
        self.schema_version
    }

    pub fn git_revision(&self) -> &str {
        self.git_revision.as_str()
    }

    pub fn config_digest(&self) -> &str {
        self.config_digest.as_str()
    }

    pub fn comparison_identity(&self) -> &ComparisonIdentity {
        &self.comparison_identity
    }

    pub fn corpus(&self) -> &CorpusIdentity {
        &self.corpus
    }

    pub fn systems(&self) -> &SystemsCatalog {
        &self.systems
    }

    pub fn query_matrix(&self) -> &QueryMatrix {
        &self.query_matrix
    }

    pub fn hardware(&self) -> &HardwareProfile {
        &self.hardware
    }

    pub const fn gate_policy(&self) -> HardGatePolicy {
        self.gate_policy
    }

    pub fn storage_growth(&self) -> &StorageGrowthSeries {
        &self.storage_growth
    }

    /// Runs the plan against injected measurements and emits a report.
    ///
    /// Every required backend × scenario cell must be present. Missing cells
    /// fail closed. All measurements share the plan comparison identity.
    pub fn run_with_injected(
        &self,
        measurements: &[InjectedScenarioMeasurement],
    ) -> SsdVectorBenchmarkResult<BenchmarkReport> {
        let mut results = Vec::new();
        let mut outcomes_map: std::collections::BTreeMap<BackendId, bool> =
            std::collections::BTreeMap::new();

        for system in self.systems.systems() {
            let backend_id = system.backend_id();
            let role = system.role();
            let mut backend_all_passed = true;

            for scenario in self.query_matrix.scenarios() {
                let measurement = measurements.iter().find(|m| {
                    m.backend_id == backend_id
                        && m.scenario_id == scenario.scenario_id()
                        && m.cache_state == scenario.cache_state()
                });
                let measurement = measurement.ok_or_else(|| {
                    SsdVectorBenchmarkError::contract(
                        SsdVectorBenchmarkDiagnosticCode::MissingMeasurement,
                    )
                })?;

                let quality = build_quality(&measurement.quality)?;
                let resources = ResourceMetrics::new(measurement.resources.clone())?;
                let gate_result = evaluate_scenario_gates(
                    ScenarioGateInput {
                        cache_state: scenario.cache_state(),
                        quality: &quality,
                        resources: &resources,
                        role,
                    },
                    &self.gate_policy,
                );
                let scenario_gate_passed = gate_result.is_ok();
                if !scenario_gate_passed {
                    backend_all_passed = false;
                }

                results.push(BackendScenarioResult::new(
                    backend_id,
                    role,
                    scenario.scenario_id(),
                    scenario.cache_state(),
                    quality,
                    resources,
                    scenario_gate_passed,
                )?);
            }

            // Record outcomes for roles that participate in suite verdict evaluation.
            // External controls are measured but do not enter the promotion map.
            if matches!(
                role,
                BackendRole::PrimaryCandidate
                    | BackendRole::Reference
                    | BackendRole::RegressionOnly
                    | BackendRole::ExactBaseline
            ) {
                outcomes_map.insert(backend_id, backend_all_passed);
            }
        }

        let backend_outcomes: Vec<BackendGateOutcome> = outcomes_map
            .into_iter()
            .map(|(backend_id, complete_gate_passed)| BackendGateOutcome {
                backend_id,
                role: backend_id.default_role(),
                complete_gate_passed,
            })
            .collect();

        let verdict = match evaluate_suite_verdict(&backend_outcomes) {
            Ok(v) => v,
            Err(err)
                if err.diagnostic_code()
                    == SsdVectorBenchmarkDiagnosticCode::RegressionOnlyCannotPromote =>
            {
                // Surface as Fail verdict with the diagnostic retained via Fail.
                GateVerdict::Fail
            }
            Err(err) => return Err(err),
        };

        // If regression-only was the only passer, force Fail (already handled).
        // If evaluate_suite_verdict returned the regression error path above.

        BenchmarkReport::new(super::report::BenchmarkReportParts {
            report_id: format!("report-{}", self.plan_id()),
            git_revision: self.git_revision().to_string(),
            config_digest: self.config_digest().to_string(),
            dataset_digest: self.corpus.dataset_digest().to_string(),
            qrels_digest: self.comparison_identity.qrels_digest().to_string(),
            hardware: self.hardware.clone(),
            comparison_identity: self.comparison_identity.clone(),
            corpus: self.corpus.clone(),
            systems: self.systems.clone(),
            query_matrix: self.query_matrix.clone(),
            results,
            storage_growth: Some(self.storage_growth.clone()),
            backend_outcomes,
            gate_policy: self.gate_policy,
            verdict,
        })
    }
}

fn build_quality(
    fields: &super::metrics::QualityMetricsFields,
) -> SsdVectorBenchmarkResult<QualityMetrics> {
    let mut observations = Vec::with_capacity(fields.observations.len());
    for entry in &fields.observations {
        observations.push(QualityMetricObservation::new(*entry)?);
    }
    QualityMetrics::new(observations)
}

/// Helper: construct a passing injected measurement for tests and stubs.
pub fn passing_injected_measurement(
    backend_id: BackendId,
    scenario_id: &str,
    cache_state: CacheState,
) -> SsdVectorBenchmarkResult<InjectedScenarioMeasurement> {
    let (p50, p95, p99) = match cache_state {
        CacheState::Cold => (20_000, 40_000, 80_000),
        CacheState::Warm | CacheState::CacheChurn => (5_000, 15_000, 40_000),
    };
    Ok(InjectedScenarioMeasurement {
        backend_id,
        scenario_id: scenario_id.to_string(),
        cache_state,
        quality: super::metrics::QualityMetricsFields {
            observations: vec![
                QualityMetricObservationFields {
                    stage: QualityStage::Candidate,
                    kind: QualityMetricKind::RecallAt10,
                    value: 0.96,
                },
                QualityMetricObservationFields {
                    stage: QualityStage::FinalRescore,
                    kind: QualityMetricKind::RecallAt10,
                    value: 0.98,
                },
                QualityMetricObservationFields {
                    stage: QualityStage::FinalRescore,
                    kind: QualityMetricKind::NdcgAt10,
                    value: 0.95,
                },
                QualityMetricObservationFields {
                    stage: QualityStage::FinalRescore,
                    kind: QualityMetricKind::FilteredAuthorizedSubsetRecall,
                    value: 0.97,
                },
            ],
        },
        resources: ResourceMetricsFields {
            latency: LatencyMicros::new(p50, p95, p99)?,
            throughput_qps_milli: 100_000,
            cgroup_memory: CgroupMemoryMeasurementFields {
                memory_current_bytes: 100_000_000,
                memory_high_bytes: 201_326_592,
                memory_max_bytes: 268_435_456,
                peak_bytes: 120_000_000,
                measured: true,
            },
            major_faults: Some(0),
            minor_faults: Some(10),
            ssd_bytes_per_query: Some(65_536),
            ssd_ops_per_query: Some(4),
            index_bytes: Some(8_192_000),
        },
    })
}

/// Helper: all required backends × local-subset scenarios with passing metrics.
pub fn all_passing_local_subset_measurements(
    plan: &LocalSubsetPlan,
) -> SsdVectorBenchmarkResult<Vec<InjectedScenarioMeasurement>> {
    let mut out = Vec::new();
    for system in plan.systems().systems() {
        for scenario in plan.query_matrix().scenarios() {
            out.push(passing_injected_measurement(
                system.backend_id(),
                scenario.scenario_id(),
                scenario.cache_state(),
            )?);
        }
    }
    Ok(out)
}

/// Re-export schema constants for adapters.
pub const fn report_schema_version() -> u32 {
    SSD_VECTOR_BENCHMARK_REPORT_SCHEMA_VERSION
}
