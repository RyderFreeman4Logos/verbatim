//! Corpus scales and identities for EVAL-SSD-001.
//!
//! Refs #382.

use serde::{Deserialize, Serialize};

use super::error::{
    SsdVectorBenchmarkDiagnosticCode, SsdVectorBenchmarkError, SsdVectorBenchmarkResult,
};
use super::identity::{ClosedLabel, ContentDigest};
use super::system::REQUIRED_VECTOR_DIMENSION;

/// Closed corpus scale classes for the program.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CorpusScale {
    /// Bible/canonical regression fixture.
    BibleCanonicalFixture,
    /// Representative enterprise document collection.
    EnterpriseDocumentCollection,
    /// Deterministic synthetic 1M-vector corpus.
    Synthetic1m,
    /// Deterministic synthetic 10M-vector corpus.
    Synthetic10m,
    /// Opaque enterprise-style corpus (Refs #271).
    OpaqueEnterpriseStyle,
    /// Small deterministic local-subset synthetic corpus.
    LocalSubsetSynthetic,
}

impl CorpusScale {
    /// Every closed corpus scale.
    pub const ALL: [Self; 6] = [
        Self::BibleCanonicalFixture,
        Self::EnterpriseDocumentCollection,
        Self::Synthetic1m,
        Self::Synthetic10m,
        Self::OpaqueEnterpriseStyle,
        Self::LocalSubsetSynthetic,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::BibleCanonicalFixture => "bible_canonical_fixture",
            Self::EnterpriseDocumentCollection => "enterprise_document_collection",
            Self::Synthetic1m => "synthetic_1m",
            Self::Synthetic10m => "synthetic_10m",
            Self::OpaqueEnterpriseStyle => "opaque_enterprise_style",
            Self::LocalSubsetSynthetic => "local_subset_synthetic",
        }
    }

    /// Whether this scale is used by the deterministic local subset plan.
    pub const fn is_local_subset(self) -> bool {
        matches!(
            self,
            Self::BibleCanonicalFixture | Self::LocalSubsetSynthetic
        )
    }
}

/// Original vector precision for final scoring.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum OriginalVectorPrecision {
    /// Full-precision float32 (required for final scoring).
    F32,
}

impl OriginalVectorPrecision {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::F32 => "f32",
        }
    }
}

/// Ground-truth configuration for the corpus.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct GroundTruthConfig {
    exact_full_dimensional: bool,
    original_precision: OriginalVectorPrecision,
    dimension: u32,
}

/// Construction fields for [`GroundTruthConfig`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GroundTruthConfigFields {
    pub exact_full_dimensional: bool,
    pub original_precision: OriginalVectorPrecision,
    pub dimension: u32,
}

impl GroundTruthConfig {
    /// Builds ground-truth config. Exact full-dimensional f32@4096 is required.
    pub fn new(fields: GroundTruthConfigFields) -> SsdVectorBenchmarkResult<Self> {
        if fields.dimension != REQUIRED_VECTOR_DIMENSION {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::DimensionReductionForbidden,
            ));
        }
        if !fields.exact_full_dimensional {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::ExactGroundTruthRequired,
            ));
        }
        if fields.original_precision != OriginalVectorPrecision::F32 {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::OriginalVectorsMustBeFullPrecision,
            ));
        }
        Ok(Self {
            exact_full_dimensional: fields.exact_full_dimensional,
            original_precision: fields.original_precision,
            dimension: fields.dimension,
        })
    }

    /// Program default: exact full-dimensional f32@4096.
    pub fn program_default() -> SsdVectorBenchmarkResult<Self> {
        Self::new(GroundTruthConfigFields {
            exact_full_dimensional: true,
            original_precision: OriginalVectorPrecision::F32,
            dimension: REQUIRED_VECTOR_DIMENSION,
        })
    }

    pub const fn exact_full_dimensional(&self) -> bool {
        self.exact_full_dimensional
    }

    pub const fn original_precision(&self) -> OriginalVectorPrecision {
        self.original_precision
    }

    pub const fn dimension(&self) -> u32 {
        self.dimension
    }
}

impl<'de> Deserialize<'de> for GroundTruthConfig {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = GroundTruthConfigFields::deserialize(deserializer)?;
        Self::new(fields).map_err(serde::de::Error::custom)
    }
}

/// Corpus identity bound into a benchmark plan/report.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct CorpusIdentity {
    scale: CorpusScale,
    corpus_id: ClosedLabel,
    dataset_digest: ContentDigest,
    vector_count: u64,
    source_count: u64,
    ground_truth: GroundTruthConfig,
}

/// Construction fields for [`CorpusIdentity`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CorpusIdentityFields {
    pub scale: CorpusScale,
    pub corpus_id: String,
    pub dataset_digest: String,
    pub vector_count: u64,
    pub source_count: u64,
    pub ground_truth: GroundTruthConfigFields,
}

impl CorpusIdentity {
    /// Builds a validated corpus identity.
    pub fn new(fields: CorpusIdentityFields) -> SsdVectorBenchmarkResult<Self> {
        if fields.vector_count == 0 || fields.source_count == 0 {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::InvalidBounds,
            ));
        }
        Ok(Self {
            scale: fields.scale,
            corpus_id: ClosedLabel::new(fields.corpus_id)?,
            dataset_digest: ContentDigest::new(fields.dataset_digest)?,
            vector_count: fields.vector_count,
            source_count: fields.source_count,
            ground_truth: GroundTruthConfig::new(fields.ground_truth)?,
        })
    }

    /// Deterministic local-subset corpus (bible fixture + small synthetic N).
    pub fn local_subset_default() -> SsdVectorBenchmarkResult<Self> {
        Self::new(CorpusIdentityFields {
            scale: CorpusScale::LocalSubsetSynthetic,
            corpus_id: "local-subset-bible-synthetic-v1".to_string(),
            dataset_digest: "a1b2c3d4e5f60718293a4b5c6d7e8f90".to_string(),
            vector_count: 1_024,
            source_count: 8,
            ground_truth: GroundTruthConfigFields {
                exact_full_dimensional: true,
                original_precision: OriginalVectorPrecision::F32,
                dimension: REQUIRED_VECTOR_DIMENSION,
            },
        })
    }

    pub const fn scale(&self) -> CorpusScale {
        self.scale
    }

    pub fn corpus_id(&self) -> &str {
        self.corpus_id.as_str()
    }

    pub fn dataset_digest(&self) -> &str {
        self.dataset_digest.as_str()
    }

    pub const fn vector_count(&self) -> u64 {
        self.vector_count
    }

    pub const fn source_count(&self) -> u64 {
        self.source_count
    }

    pub fn ground_truth(&self) -> &GroundTruthConfig {
        &self.ground_truth
    }
}

impl<'de> Deserialize<'de> for CorpusIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = CorpusIdentityFields::deserialize(deserializer)?;
        Self::new(fields).map_err(serde::de::Error::custom)
    }
}

/// Storage-growth verification series: index bytes vs N and source count.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct StorageGrowthSeries {
    /// Observed (vector_count, index_bytes) points.
    by_vector_count: Vec<StorageGrowthPoint>,
    /// Observed (source_count, index_bytes) points.
    by_source_count: Vec<StorageGrowthPoint>,
}

/// One storage-growth observation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct StorageGrowthPoint {
    /// Independent variable (N or source count).
    pub x: u64,
    /// Index bytes at that scale.
    pub index_bytes: u64,
}

/// Construction fields for [`StorageGrowthSeries`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StorageGrowthSeriesFields {
    pub by_vector_count: Vec<StorageGrowthPoint>,
    pub by_source_count: Vec<StorageGrowthPoint>,
}

impl StorageGrowthSeries {
    /// Builds a series. Both N and source-count series require at least two points.
    pub fn new(fields: StorageGrowthSeriesFields) -> SsdVectorBenchmarkResult<Self> {
        if fields.by_vector_count.len() < 2 || fields.by_source_count.len() < 2 {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::MissingStorageGrowthSeries,
            ));
        }
        for point in fields
            .by_vector_count
            .iter()
            .chain(fields.by_source_count.iter())
        {
            if point.x == 0 {
                return Err(SsdVectorBenchmarkError::contract(
                    SsdVectorBenchmarkDiagnosticCode::InvalidBounds,
                ));
            }
        }
        Ok(Self {
            by_vector_count: fields.by_vector_count,
            by_source_count: fields.by_source_count,
        })
    }

    pub fn by_vector_count(&self) -> &[StorageGrowthPoint] {
        &self.by_vector_count
    }

    pub fn by_source_count(&self) -> &[StorageGrowthPoint] {
        &self.by_source_count
    }
}

impl<'de> Deserialize<'de> for StorageGrowthSeries {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = StorageGrowthSeriesFields::deserialize(deserializer)?;
        Self::new(fields).map_err(serde::de::Error::custom)
    }
}
