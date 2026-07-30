//! Systems-under-test catalog for EVAL-SSD-001.
//!
//! Refs #382.

use serde::{Deserialize, Serialize};

use super::error::{
    SsdVectorBenchmarkDiagnosticCode, SsdVectorBenchmarkError, SsdVectorBenchmarkResult,
};
use super::identity::ClosedLabel;

/// Required full embedding dimension for this program.
pub const REQUIRED_VECTOR_DIMENSION: u32 = 4_096;

/// Closed backend identity for a system under test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendId {
    /// DiskANN3 standard SSD-native provider.
    Diskann3Standard,
    /// DiskANN3 AISAQ colocated-performance layout.
    Diskann3AisaqColocatedPerformance,
    /// DiskANN3 AISAQ colocated-scale layout.
    Diskann3AisaqColocatedScale,
    /// Exact full-dimensional flat scan baseline.
    ExactFullDimensionalFlatScan,
    /// Qdrant reference service (disk vectors / HNSW / payload + quantize/rescore).
    QdrantReference,
    /// LanceDB IVF_RQ reference.
    LancedbIvfRq,
    /// LanceDB IVF_PQ reference.
    LancedbIvfPq,
    /// SQLite scan regression baseline (not a promotion candidate by default).
    SqliteScanRegression,
    /// instant-distance HNSW regression baseline (not a promotion candidate).
    InstantDistanceHnswRegression,
    /// Optional USearch filtered/mmap HNSW control (external/control).
    UsearchHnswControl,
    /// Optional external Milvus AISAQ control (not Verbatim process budget).
    MilvusAisaqControl,
}

impl BackendId {
    /// Every closed backend identity.
    pub const ALL: [Self; 11] = [
        Self::Diskann3Standard,
        Self::Diskann3AisaqColocatedPerformance,
        Self::Diskann3AisaqColocatedScale,
        Self::ExactFullDimensionalFlatScan,
        Self::QdrantReference,
        Self::LancedbIvfRq,
        Self::LancedbIvfPq,
        Self::SqliteScanRegression,
        Self::InstantDistanceHnswRegression,
        Self::UsearchHnswControl,
        Self::MilvusAisaqControl,
    ];

    /// Stable machine-readable backend id.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Diskann3Standard => "diskann3_standard",
            Self::Diskann3AisaqColocatedPerformance => "diskann3_aisaq_colocated_performance",
            Self::Diskann3AisaqColocatedScale => "diskann3_aisaq_colocated_scale",
            Self::ExactFullDimensionalFlatScan => "exact_full_dimensional_flat_scan",
            Self::QdrantReference => "qdrant_reference",
            Self::LancedbIvfRq => "lancedb_ivf_rq",
            Self::LancedbIvfPq => "lancedb_ivf_pq",
            Self::SqliteScanRegression => "sqlite_scan_regression",
            Self::InstantDistanceHnswRegression => "instant_distance_hnsw_regression",
            Self::UsearchHnswControl => "usearch_hnsw_control",
            Self::MilvusAisaqControl => "milvus_aisaq_control",
        }
    }

    /// Default role for this backend in the EVAL-SSD-001 catalog.
    pub const fn default_role(self) -> BackendRole {
        match self {
            Self::Diskann3Standard
            | Self::Diskann3AisaqColocatedPerformance
            | Self::Diskann3AisaqColocatedScale => BackendRole::PrimaryCandidate,
            Self::ExactFullDimensionalFlatScan => BackendRole::ExactBaseline,
            Self::QdrantReference | Self::LancedbIvfRq | Self::LancedbIvfPq => {
                BackendRole::Reference
            }
            Self::SqliteScanRegression | Self::InstantDistanceHnswRegression => {
                BackendRole::RegressionOnly
            }
            Self::UsearchHnswControl | Self::MilvusAisaqControl => BackendRole::ExternalControl,
        }
    }

    /// Whether this backend is required in every complete comparison suite.
    pub const fn is_required(self) -> bool {
        !matches!(self, Self::UsearchHnswControl | Self::MilvusAisaqControl)
    }
}

/// Closed role of a system under test in the comparison suite.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendRole {
    /// Primary DiskANN3-family candidate under evaluation for promotion.
    PrimaryCandidate,
    /// Exact full-dimensional baseline used for ground truth and quality.
    ExactBaseline,
    /// Reference backend that can falsify the primary architecture decision.
    Reference,
    /// Regression-only evidence; cannot promote alone.
    RegressionOnly,
    /// External/control measurement, outside Verbatim process budget.
    ExternalControl,
}

impl BackendRole {
    /// Stable machine-readable role id.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::PrimaryCandidate => "primary_candidate",
            Self::ExactBaseline => "exact_baseline",
            Self::Reference => "reference",
            Self::RegressionOnly => "regression_only",
            Self::ExternalControl => "external_control",
        }
    }

    /// Whether this role may be a sole promotion candidate.
    pub const fn can_promote_alone(self) -> bool {
        matches!(self, Self::PrimaryCandidate)
    }

    /// Whether a complete-gate win by this role forces architecture reconsideration.
    pub const fn can_force_architecture_reconsideration(self) -> bool {
        matches!(self, Self::Reference)
    }

    /// Whether measurements count against the Verbatim process memory budget.
    pub const fn counts_against_verbatim_process_budget(self) -> bool {
        !matches!(self, Self::ExternalControl)
    }
}

/// One closed system under test entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SystemUnderTest {
    backend_id: BackendId,
    role: BackendRole,
    required: bool,
    /// Opaque closed label for provider layout / profile (not a free-form path).
    layout_profile: ClosedLabel,
}

/// Construction fields for [`SystemUnderTest`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemUnderTestFields {
    pub backend_id: BackendId,
    pub role: BackendRole,
    pub required: bool,
    pub layout_profile: String,
}

impl SystemUnderTest {
    /// Builds a validated system-under-test entry.
    pub fn new(fields: SystemUnderTestFields) -> SsdVectorBenchmarkResult<Self> {
        let layout_profile = ClosedLabel::new(fields.layout_profile)?;
        if fields.required != fields.backend_id.is_required() {
            // Required flag must match the closed catalog for known backends.
            // Allow explicit override only when the catalog says optional and
            // the caller marks required=false, or catalog required=true.
            if fields.backend_id.is_required() && !fields.required {
                return Err(SsdVectorBenchmarkError::contract(
                    SsdVectorBenchmarkDiagnosticCode::MissingRequiredBackend,
                ));
            }
        }
        if fields.role != fields.backend_id.default_role() {
            // Role must match the closed catalog defaults to keep comparisons honest.
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::InvalidBackendRole,
            ));
        }
        Ok(Self {
            backend_id: fields.backend_id,
            role: fields.role,
            required: fields.required,
            layout_profile,
        })
    }

    /// Catalog entry using the closed default role and required flag.
    pub fn catalog_default(backend_id: BackendId) -> SsdVectorBenchmarkResult<Self> {
        Self::new(SystemUnderTestFields {
            backend_id,
            role: backend_id.default_role(),
            required: backend_id.is_required(),
            layout_profile: backend_id.as_str().to_string(),
        })
    }

    pub const fn backend_id(&self) -> BackendId {
        self.backend_id
    }

    pub const fn role(&self) -> BackendRole {
        self.role
    }

    pub const fn required(&self) -> bool {
        self.required
    }

    pub fn layout_profile(&self) -> &str {
        self.layout_profile.as_str()
    }
}

impl<'de> Deserialize<'de> for SystemUnderTest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = SystemUnderTestFields::deserialize(deserializer)?;
        Self::new(fields).map_err(serde::de::Error::custom)
    }
}

/// Full systems catalog for a comparison suite.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct SystemsCatalog {
    systems: Vec<SystemUnderTest>,
}

/// Construction fields for [`SystemsCatalog`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SystemsCatalogFields {
    pub systems: Vec<SystemUnderTestFields>,
}

impl SystemsCatalog {
    /// Builds a catalog and verifies all required backends are present.
    pub fn new(systems: Vec<SystemUnderTest>) -> SsdVectorBenchmarkResult<Self> {
        if systems.is_empty() {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::MissingComponent,
            ));
        }
        for required in BackendId::ALL.iter().filter(|b| b.is_required()) {
            if !systems.iter().any(|s| s.backend_id() == *required) {
                return Err(SsdVectorBenchmarkError::contract(
                    SsdVectorBenchmarkDiagnosticCode::MissingRequiredBackend,
                ));
            }
        }
        // No duplicate backend ids.
        let mut seen = std::collections::BTreeSet::new();
        for system in &systems {
            if !seen.insert(system.backend_id()) {
                return Err(SsdVectorBenchmarkError::contract(
                    SsdVectorBenchmarkDiagnosticCode::InvalidIdentity,
                ));
            }
        }
        Ok(Self { systems })
    }

    /// Default full required catalog (optional controls omitted).
    pub fn required_defaults() -> SsdVectorBenchmarkResult<Self> {
        let mut systems = Vec::new();
        for backend in BackendId::ALL {
            if backend.is_required() {
                systems.push(SystemUnderTest::catalog_default(backend)?);
            }
        }
        Self::new(systems)
    }

    /// Local-subset catalog: required in-process-capable entries only.
    ///
    /// Live Qdrant/LanceDB/Milvus clients are out of scope; this catalog still
    /// retains their closed identities for report binding when measurements are
    /// injected as harness inputs.
    pub fn local_subset_defaults() -> SsdVectorBenchmarkResult<Self> {
        Self::required_defaults()
    }

    pub fn systems(&self) -> &[SystemUnderTest] {
        &self.systems
    }

    pub fn required_systems(&self) -> impl Iterator<Item = &SystemUnderTest> {
        self.systems.iter().filter(|s| s.required())
    }
}

impl<'de> Deserialize<'de> for SystemsCatalog {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = SystemsCatalogFields::deserialize(deserializer)?;
        let mut systems = Vec::with_capacity(fields.systems.len());
        for entry in fields.systems {
            systems.push(SystemUnderTest::new(entry).map_err(serde::de::Error::custom)?);
        }
        Self::new(systems).map_err(serde::de::Error::custom)
    }
}
