//! Closed, bounded backend attributes that are never metric labels.

use serde::{Deserialize, Serialize};

use super::{TelemetryDiagnosticCode, TelemetryError, TelemetryResult};

/// Largest permitted numeric backend knob or exact-scan cardinality value.
pub const MAX_BACKEND_ATTRIBUTE_NUMERIC_VALUE: u64 = 1 << 48;

/// Closed, backend-specific attribute keys; arbitrary labels are not accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendAttributeKey {
    DiskannSearchEffort,
    DiskannProviderLayout,
    LanceDbProbes,
    LanceDbRefinement,
    LanceDbIndexType,
    QdrantHnswEf,
    QdrantQuantization,
    QdrantOversampling,
    QdrantRescore,
    ExactScanCardinality,
}

impl BackendAttributeKey {
    /// Every allowed backend-attribute key.
    pub const ALL: [Self; 10] = [
        Self::DiskannSearchEffort,
        Self::DiskannProviderLayout,
        Self::LanceDbProbes,
        Self::LanceDbRefinement,
        Self::LanceDbIndexType,
        Self::QdrantHnswEf,
        Self::QdrantQuantization,
        Self::QdrantOversampling,
        Self::QdrantRescore,
        Self::ExactScanCardinality,
    ];

    /// Stable low-cardinality attribute name.
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::DiskannSearchEffort => "diskann_search_effort",
            Self::DiskannProviderLayout => "diskann_provider_layout",
            Self::LanceDbProbes => "lancedb_probes",
            Self::LanceDbRefinement => "lancedb_refinement",
            Self::LanceDbIndexType => "lancedb_index_type",
            Self::QdrantHnswEf => "qdrant_hnsw_ef",
            Self::QdrantQuantization => "qdrant_quantization",
            Self::QdrantOversampling => "qdrant_oversampling",
            Self::QdrantRescore => "qdrant_rescore",
            Self::ExactScanCardinality => "exact_scan_cardinality",
        }
    }
}

/// DiskANN provider/page-layout categories with fixed cardinality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DiskannProviderLayout {
    Standard,
    Aisaq,
    InMemory,
}

/// LanceDB vector-index families with fixed cardinality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanceDbIndexType {
    IvfFlat,
    IvfPq,
    Hnsw,
}

/// Qdrant quantization choices with fixed cardinality.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QdrantQuantization {
    None,
    Scalar,
    Product,
    Binary,
}

/// Closed values usable with a [`BackendAttributeKey`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BackendAttributeValue {
    Unsigned(u64),
    Boolean(bool),
    DiskannProviderLayout(DiskannProviderLayout),
    LanceDbIndexType(LanceDbIndexType),
    QdrantQuantization(QdrantQuantization),
}

/// A validated backend knob stored as a span/run attribute, never a metric label.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
pub struct BackendAttribute {
    key: BackendAttributeKey,
    value: BackendAttributeValue,
}

#[derive(Deserialize)]
struct BackendAttributeWire {
    key: BackendAttributeKey,
    value: BackendAttributeValue,
}

impl<'de> Deserialize<'de> for BackendAttribute {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = BackendAttributeWire::deserialize(deserializer)?;
        Self::new(wire.key, wire.value).map_err(serde::de::Error::custom)
    }
}

impl BackendAttribute {
    /// Creates a key/value pair only when the value matches the closed key schema.
    pub fn new(key: BackendAttributeKey, value: BackendAttributeValue) -> TelemetryResult<Self> {
        let attribute = Self { key, value };
        attribute.validate()?;
        Ok(attribute)
    }

    /// Revalidates key-specific value types and numeric bounds.
    pub fn validate(&self) -> TelemetryResult<()> {
        match (self.key, self.value) {
            (
                BackendAttributeKey::DiskannSearchEffort
                | BackendAttributeKey::LanceDbProbes
                | BackendAttributeKey::LanceDbRefinement
                | BackendAttributeKey::QdrantHnswEf
                | BackendAttributeKey::QdrantOversampling,
                BackendAttributeValue::Unsigned(value),
            ) => Self::validate_positive_numeric(value),
            (BackendAttributeKey::ExactScanCardinality, BackendAttributeValue::Unsigned(value))
                if value <= MAX_BACKEND_ATTRIBUTE_NUMERIC_VALUE =>
            {
                Ok(())
            }
            (
                BackendAttributeKey::DiskannProviderLayout,
                BackendAttributeValue::DiskannProviderLayout(_),
            )
            | (BackendAttributeKey::LanceDbIndexType, BackendAttributeValue::LanceDbIndexType(_))
            | (
                BackendAttributeKey::QdrantQuantization,
                BackendAttributeValue::QdrantQuantization(_),
            )
            | (BackendAttributeKey::QdrantRescore, BackendAttributeValue::Boolean(_)) => Ok(()),
            (_, BackendAttributeValue::Unsigned(_)) => Err(TelemetryError::contract(
                TelemetryDiagnosticCode::BackendAttributeValueOutOfBounds,
            )),
            _ => Err(TelemetryError::contract(
                TelemetryDiagnosticCode::InvalidBackendAttribute,
            )),
        }
    }

    fn validate_positive_numeric(value: u64) -> TelemetryResult<()> {
        if value == 0 || value > MAX_BACKEND_ATTRIBUTE_NUMERIC_VALUE {
            return Err(TelemetryError::contract(
                TelemetryDiagnosticCode::BackendAttributeValueOutOfBounds,
            ));
        }
        Ok(())
    }

    /// Returns the closed backend knob key.
    pub const fn key(self) -> BackendAttributeKey {
        self.key
    }

    /// Returns the validated, fixed-cardinality value form.
    pub const fn value(self) -> BackendAttributeValue {
        self.value
    }

    /// Backend attributes are permitted only on spans/run artifacts, never labels.
    pub const fn is_metric_label(self) -> bool {
        false
    }
}
