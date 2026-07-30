//! Opaque closed labels and digests for the SSD vector benchmark contract.
//!
//! Refs #382.

use std::fmt;

use serde::{Deserialize, Serialize};

use super::error::{
    SsdVectorBenchmarkDiagnosticCode, SsdVectorBenchmarkError, SsdVectorBenchmarkResult,
};

/// Maximum length of a closed identity label (bytes).
pub const MAX_CLOSED_LABEL_BYTES: usize = 128;

/// Maximum length of a hex digest string.
pub const MAX_DIGEST_HEX_BYTES: usize = 128;

/// Validated closed label: non-empty, ASCII token, no paths or free text.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
pub struct ClosedLabel(String);

impl ClosedLabel {
    /// Constructs a closed label from a caller string.
    ///
    /// Alphanumeric start is allowed so git short SHAs (e.g. `6a61787`) remain
    /// valid opaque tokens.
    pub fn new(value: impl Into<String>) -> SsdVectorBenchmarkResult<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_CLOSED_LABEL_BYTES {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::InvalidIdentity,
            ));
        }
        let valid = value
            .bytes()
            .next()
            .is_some_and(|b| b.is_ascii_alphanumeric())
            && value.bytes().all(|b| {
                b.is_ascii_alphanumeric() || b == b'-' || b == b'_' || b == b'.' || b == b':'
            });
        if !valid {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::InvalidIdentity,
            ));
        }
        // Reject path-like fragments that embed user content.
        if value.contains("..") || value.contains('/') || value.contains('\\') {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ClosedLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ClosedLabel({})", self.0)
    }
}

impl fmt::Display for ClosedLabel {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        // Labels are closed tokens, safe to render.
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ClosedLabel {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Hex-encoded content digest used for comparison identity binding.
#[derive(Clone, PartialEq, Eq, Hash, Serialize)]
pub struct ContentDigest(String);

impl ContentDigest {
    /// Constructs a digest from a non-empty hex-like opaque token.
    pub fn new(value: impl Into<String>) -> SsdVectorBenchmarkResult<Self> {
        let value = value.into();
        if value.is_empty() || value.len() > MAX_DIGEST_HEX_BYTES {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::InvalidIdentity,
            ));
        }
        let valid = value
            .bytes()
            .all(|b| b.is_ascii_hexdigit() || b == b'_' || b == b'-');
        if !valid {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(Self(value))
    }

    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(formatter, "ContentDigest({})", self.0)
    }
}

impl fmt::Display for ContentDigest {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(&self.0)
    }
}

impl<'de> Deserialize<'de> for ContentDigest {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let value = String::deserialize(deserializer)?;
        Self::new(value).map_err(serde::de::Error::custom)
    }
}

/// Cross-backend comparison identity: identical vectors, filters, budgets, qrels.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ComparisonIdentity {
    vectors_digest: ContentDigest,
    filters_digest: ContentDigest,
    budgets_digest: ContentDigest,
    qrels_digest: ContentDigest,
    final_scoring_policy: ClosedLabel,
    dimension: u32,
}

/// Construction fields for [`ComparisonIdentity`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonIdentityFields {
    pub vectors_digest: String,
    pub filters_digest: String,
    pub budgets_digest: String,
    pub qrels_digest: String,
    pub final_scoring_policy: String,
    pub dimension: u32,
}

impl ComparisonIdentity {
    /// Builds a comparison identity. Dimension must be 4096.
    pub fn new(fields: ComparisonIdentityFields) -> SsdVectorBenchmarkResult<Self> {
        if fields.dimension != super::system::REQUIRED_VECTOR_DIMENSION {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::DimensionReductionForbidden,
            ));
        }
        Ok(Self {
            vectors_digest: ContentDigest::new(fields.vectors_digest)?,
            filters_digest: ContentDigest::new(fields.filters_digest)?,
            budgets_digest: ContentDigest::new(fields.budgets_digest)?,
            qrels_digest: ContentDigest::new(fields.qrels_digest)?,
            final_scoring_policy: ClosedLabel::new(fields.final_scoring_policy)?,
            dimension: fields.dimension,
        })
    }

    /// Returns an error when another identity is not equal for comparison.
    pub fn require_equal(&self, other: &Self) -> SsdVectorBenchmarkResult<()> {
        if self != other {
            return Err(SsdVectorBenchmarkError::contract(
                SsdVectorBenchmarkDiagnosticCode::UnequalComparisonIdentity,
            ));
        }
        Ok(())
    }

    pub fn vectors_digest(&self) -> &str {
        self.vectors_digest.as_str()
    }

    pub fn filters_digest(&self) -> &str {
        self.filters_digest.as_str()
    }

    pub fn budgets_digest(&self) -> &str {
        self.budgets_digest.as_str()
    }

    pub fn qrels_digest(&self) -> &str {
        self.qrels_digest.as_str()
    }

    pub fn final_scoring_policy(&self) -> &str {
        self.final_scoring_policy.as_str()
    }

    pub const fn dimension(&self) -> u32 {
        self.dimension
    }
}

impl<'de> Deserialize<'de> for ComparisonIdentity {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let fields = ComparisonIdentityFields::deserialize(deserializer)?;
        Self::new(fields).map_err(serde::de::Error::custom)
    }
}
