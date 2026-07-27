//! Filter predicates that must be pushed into candidate generation.

use serde::{Deserialize, Serialize};

use super::{VectorSearchDiagnosticCode, VectorSearchError, VectorSearchResult};

/// Lifecycle state permitted in a retrieval predicate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LifecycleState {
    Active,
    Archived,
    Retained,
}

/// Closed typed metadata values permitted in predicates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "type", content = "value")]
pub enum TypedMetadataValue {
    String(String),
    Integer(i64),
    Boolean(bool),
    Float(f64),
}

impl TypedMetadataValue {
    fn validate(&self) -> VectorSearchResult<()> {
        match self {
            Self::String(value) if !is_bounded_value(value) => Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::FilterUnsupported,
            )),
            Self::Float(value) if !value.is_finite() => Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::FilterUnsupported,
            )),
            _ => Ok(()),
        }
    }
}

/// Enterprise predicates that narrow candidate generation before traversal.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum FilterPredicate {
    Source {
        source_id: String,
    },
    Collection {
        collection_id: String,
    },
    Tenant {
        tenant_id: String,
    },
    Acl {
        principal_or_group: String,
    },
    Lifecycle {
        lifecycle: LifecycleState,
    },
    Language {
        language: String,
    },
    DateRange {
        start_unix_ms: i64,
        end_unix_ms: i64,
    },
    MetadataEq {
        key: String,
        value: TypedMetadataValue,
    },
}

impl FilterPredicate {
    pub fn source(value: impl Into<String>) -> VectorSearchResult<Self> {
        Self::named(|source_id| Self::Source { source_id }, value)
    }

    pub fn collection(value: impl Into<String>) -> VectorSearchResult<Self> {
        Self::named(|collection_id| Self::Collection { collection_id }, value)
    }

    pub fn tenant(value: impl Into<String>) -> VectorSearchResult<Self> {
        Self::named(|tenant_id| Self::Tenant { tenant_id }, value)
    }

    pub fn acl(value: impl Into<String>) -> VectorSearchResult<Self> {
        Self::named(|principal_or_group| Self::Acl { principal_or_group }, value)
    }

    pub fn lifecycle(lifecycle: LifecycleState) -> Self {
        Self::Lifecycle { lifecycle }
    }

    pub fn language(value: impl Into<String>) -> VectorSearchResult<Self> {
        Self::named(|language| Self::Language { language }, value)
    }

    pub fn date_range(start_unix_ms: i64, end_unix_ms: i64) -> VectorSearchResult<Self> {
        let filter = Self::DateRange {
            start_unix_ms,
            end_unix_ms,
        };
        filter.validate()?;
        Ok(filter)
    }

    pub fn metadata_eq(
        key: impl Into<String>,
        value: TypedMetadataValue,
    ) -> VectorSearchResult<Self> {
        let filter = Self::MetadataEq {
            key: key.into(),
            value,
        };
        filter.validate()?;
        Ok(filter)
    }

    fn named(
        construct: impl FnOnce(String) -> Self,
        value: impl Into<String>,
    ) -> VectorSearchResult<Self> {
        let filter = construct(value.into());
        filter.validate()?;
        Ok(filter)
    }

    pub fn validate(&self) -> VectorSearchResult<()> {
        match self {
            Self::Source { source_id }
            | Self::Collection {
                collection_id: source_id,
            }
            | Self::Tenant {
                tenant_id: source_id,
            }
            | Self::Acl {
                principal_or_group: source_id,
            }
            | Self::Language {
                language: source_id,
            } if !is_bounded_value(source_id) => Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::FilterUnsupported,
            )),
            Self::DateRange {
                start_unix_ms,
                end_unix_ms,
            } if start_unix_ms > end_unix_ms => Err(VectorSearchError::contract(
                VectorSearchDiagnosticCode::FilterUnsupported,
            )),
            Self::MetadataEq { key, value } if !is_bounded_value(key) => Err(
                VectorSearchError::contract(VectorSearchDiagnosticCode::FilterUnsupported),
            ),
            Self::MetadataEq { value, .. } => value.validate(),
            _ => Ok(()),
        }
    }
}

fn is_bounded_value(value: &str) -> bool {
    !value.is_empty() && value.len() <= 256 && value.is_ascii()
}
