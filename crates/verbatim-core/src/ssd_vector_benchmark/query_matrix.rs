//! Query matrix dimensions for EVAL-SSD-001.
//!
//! Refs #382.

use serde::{Deserialize, Serialize};

use super::error::{
    SsdVectorBenchmarkDiagnosticCode, SsdVectorBenchmarkError, SsdVectorBenchmarkResult,
};
use super::identity::ClosedLabel;

/// Closed query class vocabulary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryClass {
    Semantic,
    Lexical,
    ExactIdentifierReference,
    Phrase,
    Multilingual,
    MixedLanguage,
    MultiHop,
}

impl QueryClass {
    pub const ALL: [Self; 7] = [
        Self::Semantic,
        Self::Lexical,
        Self::ExactIdentifierReference,
        Self::Phrase,
        Self::Multilingual,
        Self::MixedLanguage,
        Self::MultiHop,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Semantic => "semantic",
            Self::Lexical => "lexical",
            Self::ExactIdentifierReference => "exact_identifier_reference",
            Self::Phrase => "phrase",
            Self::Multilingual => "multilingual",
            Self::MixedLanguage => "mixed_language",
            Self::MultiHop => "multi_hop",
        }
    }
}

/// Closed filter selectivity bands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterSelectivity {
    Full100,
    TenPercent,
    OnePercent,
    PointOnePercent,
    PointZeroOnePercent,
    SingleSourceDocument,
    ZeroAuthorized,
}

impl FilterSelectivity {
    pub const ALL: [Self; 7] = [
        Self::Full100,
        Self::TenPercent,
        Self::OnePercent,
        Self::PointOnePercent,
        Self::PointZeroOnePercent,
        Self::SingleSourceDocument,
        Self::ZeroAuthorized,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Full100 => "full_100",
            Self::TenPercent => "ten_percent",
            Self::OnePercent => "one_percent",
            Self::PointOnePercent => "point_one_percent",
            Self::PointZeroOnePercent => "point_zero_one_percent",
            Self::SingleSourceDocument => "single_source_document",
            Self::ZeroAuthorized => "zero_authorized",
        }
    }
}

/// Closed concurrency levels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ConcurrencyLevel {
    One,
    Eight,
    ThirtyTwo,
    Saturation,
}

impl ConcurrencyLevel {
    pub const ALL: [Self; 4] = [Self::One, Self::Eight, Self::ThirtyTwo, Self::Saturation];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::One => "one",
            Self::Eight => "eight",
            Self::ThirtyTwo => "thirty_two",
            Self::Saturation => "saturation",
        }
    }
}

/// Cache state is first-class, not an optional note.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CacheState {
    Cold,
    Warm,
    CacheChurn,
}

impl CacheState {
    pub const ALL: [Self; 3] = [Self::Cold, Self::Warm, Self::CacheChurn];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Cold => "cold",
            Self::Warm => "warm",
            Self::CacheChurn => "cache_churn",
        }
    }

    /// Whether this state is required for every complete acceptance suite.
    pub const fn is_required_for_acceptance(self) -> bool {
        matches!(self, Self::Cold | Self::Warm)
    }
}

/// Update / lifecycle state of the index under measurement.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UpdateState {
    ReadOnly,
    MixedUpdates,
    SourceReplacement,
    DeleteHeavy,
    Compaction,
}

impl UpdateState {
    pub const ALL: [Self; 5] = [
        Self::ReadOnly,
        Self::MixedUpdates,
        Self::SourceReplacement,
        Self::DeleteHeavy,
        Self::Compaction,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ReadOnly => "read_only",
            Self::MixedUpdates => "mixed_updates",
            Self::SourceReplacement => "source_replacement",
            Self::DeleteHeavy => "delete_heavy",
            Self::Compaction => "compaction",
        }
    }
}

/// One closed query-matrix scenario cell.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueryScenario {
    scenario_id: ClosedLabel,
    query_class: QueryClass,
    filter_selectivity: FilterSelectivity,
    concurrency: ConcurrencyLevel,
    cache_state: CacheState,
    update_state: UpdateState,
    query_count: u32,
}

/// Construction fields for [`QueryScenario`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryScenarioFields {
    pub scenario_id: String,
    pub query_class: QueryClass,
    pub filter_selectivity: FilterSelectivity,
    pub concurrency: ConcurrencyLevel,
    pub cache_state: CacheState,
    pub update_state: UpdateState,
    pub query_count: u32,
}

impl QueryScenario {
    /// Builds a validated query scenario.
    pub fn new(fields: QueryScenarioFields) -> SsdVectorBenchmarkResult<Self> {
        if fields.query_count == 0 {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::InvalidBounds,
            ));
        }
        Ok(Self {
            scenario_id: ClosedLabel::new(fields.scenario_id)?,
            query_class: fields.query_class,
            filter_selectivity: fields.filter_selectivity,
            concurrency: fields.concurrency,
            cache_state: fields.cache_state,
            update_state: fields.update_state,
            query_count: fields.query_count,
        })
    }

    pub fn scenario_id(&self) -> &str {
        self.scenario_id.as_str()
    }

    pub const fn query_class(&self) -> QueryClass {
        self.query_class
    }

    pub const fn filter_selectivity(&self) -> FilterSelectivity {
        self.filter_selectivity
    }

    pub const fn concurrency(&self) -> ConcurrencyLevel {
        self.concurrency
    }

    pub const fn cache_state(&self) -> CacheState {
        self.cache_state
    }

    pub const fn update_state(&self) -> UpdateState {
        self.update_state
    }

    pub const fn query_count(&self) -> u32 {
        self.query_count
    }
}

impl<'de> Deserialize<'de> for QueryScenario {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = QueryScenarioFields::deserialize(deserializer)?;
        Self::new(fields).map_err(serde::de::Error::custom)
    }
}

/// Full query matrix for a plan; must include cold and warm for acceptance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QueryMatrix {
    scenarios: Vec<QueryScenario>,
}

/// Construction fields for [`QueryMatrix`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryMatrixFields {
    pub scenarios: Vec<QueryScenarioFields>,
}

impl QueryMatrix {
    /// Builds a matrix. Cold and warm cache states are required.
    pub fn new(scenarios: Vec<QueryScenario>) -> SsdVectorBenchmarkResult<Self> {
        if scenarios.is_empty() {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::MissingComponent,
            ));
        }
        let has_cold = scenarios
            .iter()
            .any(|s| s.cache_state() == CacheState::Cold);
        let has_warm = scenarios
            .iter()
            .any(|s| s.cache_state() == CacheState::Warm);
        if !has_cold || !has_warm {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::MissingColdWarmCacheState,
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        for scenario in &scenarios {
            if !seen.insert(scenario.scenario_id().to_string()) {
                return Err(SsdVectorBenchmarkError::contract(
                    SsdVectorBenchmarkDiagnosticCode::InvalidIdentity,
                ));
            }
        }
        Ok(Self { scenarios })
    }

    /// Deterministic local-subset matrix (semantic + cold/warm + read-only).
    pub fn local_subset_default() -> SsdVectorBenchmarkResult<Self> {
        let cold = QueryScenario::new(QueryScenarioFields {
            scenario_id: "local-semantic-broad-cold".to_string(),
            query_class: QueryClass::Semantic,
            filter_selectivity: FilterSelectivity::Full100,
            concurrency: ConcurrencyLevel::One,
            cache_state: CacheState::Cold,
            update_state: UpdateState::ReadOnly,
            query_count: 32,
        })?;
        let warm = QueryScenario::new(QueryScenarioFields {
            scenario_id: "local-semantic-broad-warm".to_string(),
            query_class: QueryClass::Semantic,
            filter_selectivity: FilterSelectivity::Full100,
            concurrency: ConcurrencyLevel::One,
            cache_state: CacheState::Warm,
            update_state: UpdateState::ReadOnly,
            query_count: 32,
        })?;
        Self::new(vec![cold, warm])
    }

    pub fn scenarios(&self) -> &[QueryScenario] {
        &self.scenarios
    }
}

impl<'de> Deserialize<'de> for QueryMatrix {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = QueryMatrixFields::deserialize(deserializer)?;
        let mut scenarios = Vec::with_capacity(fields.scenarios.len());
        for entry in fields.scenarios {
            scenarios.push(QueryScenario::new(entry).map_err(serde::de::Error::custom)?);
        }
        Self::new(scenarios).map_err(serde::de::Error::custom)
    }
}
