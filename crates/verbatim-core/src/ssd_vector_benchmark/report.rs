//! Machine-readable and Markdown local-subset report emission.
//!
//! Refs #382.

use serde::{Deserialize, Serialize};

use super::corpus::{CorpusIdentity, StorageGrowthSeries};
use super::error::{
    SsdVectorBenchmarkDiagnosticCode, SsdVectorBenchmarkError, SsdVectorBenchmarkResult,
};
use super::gate::{BackendGateOutcome, GateVerdict, HardGatePolicy};
use super::identity::{ClosedLabel, ComparisonIdentity, ContentDigest};
use super::metrics::{QualityMetrics, ResourceMetrics};
use super::query_matrix::{CacheState, QueryMatrix};
use super::system::{BackendId, BackendRole, SystemsCatalog};

/// Contract schema version for SSD vector benchmark reports.
pub const SSD_VECTOR_BENCHMARK_REPORT_SCHEMA_VERSION: u32 = 1;

/// Hardware profile binding required for absolute latency evaluation.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct HardwareProfile {
    profile_id: ClosedLabel,
    cpu_label: ClosedLabel,
    ram_limit_bytes: u64,
    nvme_model: ClosedLabel,
    filesystem: ClosedLabel,
    kernel: ClosedLabel,
    io_mode: ClosedLabel,
    compiler: ClosedLabel,
}

/// Construction fields for [`HardwareProfile`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HardwareProfileFields {
    pub profile_id: String,
    pub cpu_label: String,
    pub ram_limit_bytes: u64,
    pub nvme_model: String,
    pub filesystem: String,
    pub kernel: String,
    pub io_mode: String,
    pub compiler: String,
}

impl HardwareProfile {
    /// Builds a hardware profile. All labels must be non-empty closed tokens.
    pub fn new(fields: HardwareProfileFields) -> SsdVectorBenchmarkResult<Self> {
        if fields.ram_limit_bytes == 0 {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::IncompleteHardwareProfile,
            ));
        }
        Ok(Self {
            profile_id: ClosedLabel::new(fields.profile_id)?,
            cpu_label: ClosedLabel::new(fields.cpu_label)?,
            ram_limit_bytes: fields.ram_limit_bytes,
            nvme_model: ClosedLabel::new(fields.nvme_model)?,
            filesystem: ClosedLabel::new(fields.filesystem)?,
            kernel: ClosedLabel::new(fields.kernel)?,
            io_mode: ClosedLabel::new(fields.io_mode)?,
            compiler: ClosedLabel::new(fields.compiler)?,
        })
    }

    /// Deterministic local-subset hardware profile placeholder.
    pub fn local_subset_default() -> SsdVectorBenchmarkResult<Self> {
        Self::new(HardwareProfileFields {
            profile_id: "local-subset-dev-profile-v1".to_string(),
            cpu_label: "x86_64-generic".to_string(),
            ram_limit_bytes: 268_435_456,
            nvme_model: "local-nvme-generic".to_string(),
            filesystem: "ext4".to_string(),
            kernel: "linux-6.x".to_string(),
            io_mode: "buffered".to_string(),
            compiler: "rustc-1.97.1".to_string(),
        })
    }

    pub fn profile_id(&self) -> &str {
        self.profile_id.as_str()
    }

    pub fn cpu_label(&self) -> &str {
        self.cpu_label.as_str()
    }

    pub const fn ram_limit_bytes(&self) -> u64 {
        self.ram_limit_bytes
    }

    pub fn nvme_model(&self) -> &str {
        self.nvme_model.as_str()
    }

    pub fn filesystem(&self) -> &str {
        self.filesystem.as_str()
    }

    pub fn kernel(&self) -> &str {
        self.kernel.as_str()
    }

    pub fn io_mode(&self) -> &str {
        self.io_mode.as_str()
    }

    pub fn compiler(&self) -> &str {
        self.compiler.as_str()
    }
}

impl<'de> Deserialize<'de> for HardwareProfile {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = HardwareProfileFields::deserialize(deserializer)?;
        Self::new(fields).map_err(serde::de::Error::custom)
    }
}

/// Per-backend scenario result with quality + resource metrics.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BackendScenarioResult {
    backend_id: BackendId,
    role: BackendRole,
    scenario_id: ClosedLabel,
    cache_state: CacheState,
    quality: QualityMetrics,
    resources: ResourceMetrics,
    scenario_gate_passed: bool,
}

/// Construction fields for [`BackendScenarioResult`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BackendScenarioResultFields {
    pub backend_id: BackendId,
    pub role: BackendRole,
    pub scenario_id: String,
    pub cache_state: CacheState,
    pub quality: super::metrics::QualityMetricsFields,
    pub resources: super::metrics::ResourceMetricsFields,
    pub scenario_gate_passed: bool,
}

impl BackendScenarioResult {
    /// Builds a backend scenario result.
    pub fn new(
        backend_id: BackendId,
        role: BackendRole,
        scenario_id: impl Into<String>,
        cache_state: CacheState,
        quality: QualityMetrics,
        resources: ResourceMetrics,
        scenario_gate_passed: bool,
    ) -> SsdVectorBenchmarkResult<Self> {
        if role != backend_id.default_role() {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::InvalidBackendRole,
            ));
        }
        Ok(Self {
            backend_id,
            role,
            scenario_id: ClosedLabel::new(scenario_id)?,
            cache_state,
            quality,
            resources,
            scenario_gate_passed,
        })
    }

    pub const fn backend_id(&self) -> BackendId {
        self.backend_id
    }

    pub const fn role(&self) -> BackendRole {
        self.role
    }

    pub fn scenario_id(&self) -> &str {
        self.scenario_id.as_str()
    }

    pub const fn cache_state(&self) -> CacheState {
        self.cache_state
    }

    pub fn quality(&self) -> &QualityMetrics {
        &self.quality
    }

    pub fn resources(&self) -> &ResourceMetrics {
        &self.resources
    }

    pub const fn scenario_gate_passed(&self) -> bool {
        self.scenario_gate_passed
    }
}

impl<'de> Deserialize<'de> for BackendScenarioResult {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = BackendScenarioResultFields::deserialize(deserializer)?;
        let quality = QualityMetrics::new(
            fields
                .quality
                .observations
                .into_iter()
                .map(|o| {
                    super::metrics::QualityMetricObservation::new(o)
                        .map_err(serde::de::Error::custom)
                })
                .collect::<Result<Vec<_>, _>>()?,
        )
        .map_err(serde::de::Error::custom)?;
        let resources = ResourceMetrics::new(fields.resources).map_err(serde::de::Error::custom)?;
        Self::new(
            fields.backend_id,
            fields.role,
            fields.scenario_id,
            fields.cache_state,
            quality,
            resources,
            fields.scenario_gate_passed,
        )
        .map_err(serde::de::Error::custom)
    }
}

/// Full machine-readable local-subset (or full-suite) report.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct BenchmarkReport {
    report_id: ClosedLabel,
    schema_version: u32,
    git_revision: ClosedLabel,
    config_digest: ContentDigest,
    dataset_digest: ContentDigest,
    qrels_digest: ContentDigest,
    hardware: HardwareProfile,
    comparison_identity: ComparisonIdentity,
    corpus: CorpusIdentity,
    systems: SystemsCatalog,
    query_matrix: QueryMatrix,
    results: Vec<BackendScenarioResult>,
    storage_growth: Option<StorageGrowthSeries>,
    backend_outcomes: Vec<BackendGateOutcome>,
    gate_policy: HardGatePolicy,
    verdict: GateVerdict,
}

/// Construction fields for [`BenchmarkReport`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BenchmarkReportFields {
    pub report_id: String,
    pub schema_version: u32,
    pub git_revision: String,
    pub config_digest: String,
    pub dataset_digest: String,
    pub qrels_digest: String,
    pub hardware: HardwareProfileFields,
    pub comparison_identity: super::identity::ComparisonIdentityFields,
    pub corpus: super::corpus::CorpusIdentityFields,
    pub systems: super::system::SystemsCatalogFields,
    pub query_matrix: super::query_matrix::QueryMatrixFields,
    pub results: Vec<BackendScenarioResultFields>,
    pub storage_growth: Option<super::corpus::StorageGrowthSeriesFields>,
    pub backend_outcomes: Vec<BackendGateOutcome>,
    pub gate_policy: HardGatePolicy,
    pub verdict: GateVerdict,
}

/// Validated construction parts for [`BenchmarkReport`] (avoids a 15-arg constructor).
#[derive(Debug, Clone)]
pub struct BenchmarkReportParts {
    pub report_id: String,
    pub git_revision: String,
    pub config_digest: String,
    pub dataset_digest: String,
    pub qrels_digest: String,
    pub hardware: HardwareProfile,
    pub comparison_identity: ComparisonIdentity,
    pub corpus: CorpusIdentity,
    pub systems: SystemsCatalog,
    pub query_matrix: QueryMatrix,
    pub results: Vec<BackendScenarioResult>,
    pub storage_growth: Option<StorageGrowthSeries>,
    pub backend_outcomes: Vec<BackendGateOutcome>,
    pub gate_policy: HardGatePolicy,
    pub verdict: GateVerdict,
}

impl BenchmarkReport {
    /// Builds a validated report. Digests and identities must be non-empty.
    pub fn new(parts: BenchmarkReportParts) -> SsdVectorBenchmarkResult<Self> {
        if parts.results.is_empty() {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::MissingComponent,
            ));
        }
        if parts.gate_policy.require_storage_growth_series && parts.storage_growth.is_none() {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::MissingStorageGrowthSeries,
            ));
        }
        // Cold/warm must appear in results when required.
        if parts.gate_policy.require_cold_and_warm {
            let has_cold = parts
                .results
                .iter()
                .any(|r| r.cache_state() == CacheState::Cold);
            let has_warm = parts
                .results
                .iter()
                .any(|r| r.cache_state() == CacheState::Warm);
            if !has_cold || !has_warm {
                return Err(SsdVectorBenchmarkError::contract(
                    SsdVectorBenchmarkDiagnosticCode::MissingColdWarmCacheState,
                ));
            }
        }
        // All results must share the report comparison identity dimension.
        if parts.comparison_identity.dimension() != parts.corpus.ground_truth().dimension() {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::UnequalComparisonIdentity,
            ));
        }
        Ok(Self {
            report_id: ClosedLabel::new(parts.report_id)?,
            schema_version: SSD_VECTOR_BENCHMARK_REPORT_SCHEMA_VERSION,
            git_revision: ClosedLabel::new(parts.git_revision)?,
            config_digest: ContentDigest::new(parts.config_digest)?,
            dataset_digest: ContentDigest::new(parts.dataset_digest)?,
            qrels_digest: ContentDigest::new(parts.qrels_digest)?,
            hardware: parts.hardware,
            comparison_identity: parts.comparison_identity,
            corpus: parts.corpus,
            systems: parts.systems,
            query_matrix: parts.query_matrix,
            results: parts.results,
            storage_growth: parts.storage_growth,
            backend_outcomes: parts.backend_outcomes,
            gate_policy: parts.gate_policy,
            verdict: parts.verdict,
        })
    }

    pub fn report_id(&self) -> &str {
        self.report_id.as_str()
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

    pub fn dataset_digest(&self) -> &str {
        self.dataset_digest.as_str()
    }

    pub fn qrels_digest(&self) -> &str {
        self.qrels_digest.as_str()
    }

    pub fn hardware(&self) -> &HardwareProfile {
        &self.hardware
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

    pub fn results(&self) -> &[BackendScenarioResult] {
        &self.results
    }

    pub fn storage_growth(&self) -> Option<&StorageGrowthSeries> {
        self.storage_growth.as_ref()
    }

    pub fn backend_outcomes(&self) -> &[BackendGateOutcome] {
        &self.backend_outcomes
    }

    pub const fn gate_policy(&self) -> HardGatePolicy {
        self.gate_policy
    }

    pub const fn verdict(&self) -> GateVerdict {
        self.verdict
    }

    /// Emits a Markdown summary suitable for human review (no secrets/paths).
    pub fn to_markdown(&self) -> String {
        let mut out = String::new();
        out.push_str("# SSD vector benchmark report\n\n");
        out.push_str(&format!("- report_id: `{}`\n", self.report_id()));
        out.push_str(&format!("- schema_version: {}\n", self.schema_version()));
        out.push_str(&format!("- git_revision: `{}`\n", self.git_revision()));
        out.push_str(&format!("- config_digest: `{}`\n", self.config_digest()));
        out.push_str(&format!("- dataset_digest: `{}`\n", self.dataset_digest()));
        out.push_str(&format!("- qrels_digest: `{}`\n", self.qrels_digest()));
        out.push_str(&format!(
            "- hardware_profile: `{}`\n",
            self.hardware().profile_id()
        ));
        out.push_str(&format!(
            "- dimension: {}\n",
            self.comparison_identity().dimension()
        ));
        out.push_str(&format!(
            "- corpus: `{}` (N={}, sources={})\n",
            self.corpus().corpus_id(),
            self.corpus().vector_count(),
            self.corpus().source_count()
        ));
        out.push_str(&format!("- verdict: **{}**\n\n", self.verdict().as_str()));

        out.push_str("## Backend outcomes\n\n");
        out.push_str("| backend | role | complete_gate_passed |\n");
        out.push_str("| --- | --- | --- |\n");
        for outcome in &self.backend_outcomes {
            out.push_str(&format!(
                "| `{}` | `{}` | {} |\n",
                outcome.backend_id.as_str(),
                outcome.role.as_str(),
                outcome.complete_gate_passed
            ));
        }
        out.push('\n');

        out.push_str("## Scenario results\n\n");
        out.push_str("| backend | scenario | cache | gate_passed |\n");
        out.push_str("| --- | --- | --- | --- |\n");
        for result in &self.results {
            out.push_str(&format!(
                "| `{}` | `{}` | `{}` | {} |\n",
                result.backend_id().as_str(),
                result.scenario_id(),
                result.cache_state().as_str(),
                result.scenario_gate_passed()
            ));
        }
        out.push('\n');

        out.push_str("## Notes\n\n");
        out.push_str("- This report is a contract harness artifact (Refs #382 / EVAL-SSD-001).\n");
        out.push_str(
            "- Live multi-backend runners are out of scope; measurements may be injected.\n",
        );
        out.push_str("- A reference complete-gate win forces architecture reconsideration.\n");
        out
    }
}

impl<'de> Deserialize<'de> for BenchmarkReport {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = BenchmarkReportFields::deserialize(deserializer)?;
        if fields.schema_version != SSD_VECTOR_BENCHMARK_REPORT_SCHEMA_VERSION {
            return Err(serde::de::Error::custom(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::DeserializationRejected,
            )));
        }
        let hardware = HardwareProfile::new(fields.hardware).map_err(serde::de::Error::custom)?;
        let comparison_identity = ComparisonIdentity::new(fields.comparison_identity)
            .map_err(serde::de::Error::custom)?;
        let corpus = CorpusIdentity::new(fields.corpus).map_err(serde::de::Error::custom)?;
        let systems = {
            let mut systems = Vec::with_capacity(fields.systems.systems.len());
            for entry in fields.systems.systems {
                systems.push(
                    super::system::SystemUnderTest::new(entry).map_err(serde::de::Error::custom)?,
                );
            }
            SystemsCatalog::new(systems).map_err(serde::de::Error::custom)?
        };
        let query_matrix = {
            let mut scenarios = Vec::with_capacity(fields.query_matrix.scenarios.len());
            for entry in fields.query_matrix.scenarios {
                scenarios.push(
                    super::query_matrix::QueryScenario::new(entry)
                        .map_err(serde::de::Error::custom)?,
                );
            }
            QueryMatrix::new(scenarios).map_err(serde::de::Error::custom)?
        };
        let mut results = Vec::with_capacity(fields.results.len());
        for entry in fields.results {
            let quality = QualityMetrics::new(
                entry
                    .quality
                    .observations
                    .into_iter()
                    .map(|o| {
                        super::metrics::QualityMetricObservation::new(o)
                            .map_err(serde::de::Error::custom)
                    })
                    .collect::<Result<Vec<_>, _>>()?,
            )
            .map_err(serde::de::Error::custom)?;
            let resources =
                ResourceMetrics::new(entry.resources).map_err(serde::de::Error::custom)?;
            results.push(
                BackendScenarioResult::new(
                    entry.backend_id,
                    entry.role,
                    entry.scenario_id,
                    entry.cache_state,
                    quality,
                    resources,
                    entry.scenario_gate_passed,
                )
                .map_err(serde::de::Error::custom)?,
            );
        }
        let storage_growth = match fields.storage_growth {
            Some(s) => Some(StorageGrowthSeries::new(s).map_err(serde::de::Error::custom)?),
            None => None,
        };
        Self::new(BenchmarkReportParts {
            report_id: fields.report_id,
            git_revision: fields.git_revision,
            config_digest: fields.config_digest,
            dataset_digest: fields.dataset_digest,
            qrels_digest: fields.qrels_digest,
            hardware,
            comparison_identity,
            corpus,
            systems,
            query_matrix,
            results,
            storage_growth,
            backend_outcomes: fields.backend_outcomes,
            gate_policy: fields.gate_policy,
            verdict: fields.verdict,
        })
        .map_err(serde::de::Error::custom)
    }
}
