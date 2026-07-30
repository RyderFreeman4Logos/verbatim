//! Multi-source / collection / tenant / ACL filter contract for Qdrant.

use serde::{Deserialize, Serialize};

use super::{
    PayloadIndexKind, PayloadIndexPlan, QdrantBackendDiagnosticCode, QdrantBackendError,
    QdrantBackendResult,
};

const MAX_FILTER_CLAUSES: usize = 32;
const MAX_CLAUSE_VALUE_LEN: usize = 256;

/// Filter strictness. Strict filters must be native or fail closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterStrictness {
    /// Unsupported native filters fail closed (no silent global ANN + post-filter).
    StrictNativeOrFailClosed,
    /// Best-effort filters may degrade only when the plan explicitly allows it.
    BestEffort,
}

/// One filter clause applied during candidate generation.
///
/// Field names are closed and bound to payload-index requirements: each variant
/// maps to a fixed indexed field + kind that must be present in the plan.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterClause {
    Source {
        value: String,
    },
    Collection {
        value: String,
    },
    Tenant {
        value: String,
    },
    Acl {
        value: String,
    },
    Lifecycle {
        value: String,
    },
    /// Inclusive integer range predicate; requires an IntegerRange payload index.
    IntegerRange {
        field: String,
        min: i64,
        max: i64,
    },
    /// Inclusive datetime-range predicate (unix micros); requires DatetimeRange index.
    DatetimeRange {
        field: String,
        min: i64,
        max: i64,
    },
}

impl FilterClause {
    fn validate(&self) -> QdrantBackendResult<()> {
        match self {
            Self::Source { value }
            | Self::Collection { value }
            | Self::Tenant { value }
            | Self::Acl { value }
            | Self::Lifecycle { value } => {
                if value.is_empty() || value.len() > MAX_CLAUSE_VALUE_LEN {
                    return Err(QdrantBackendError::contract(
                        QdrantBackendDiagnosticCode::InvalidFilterContract,
                    ));
                }
            }
            Self::IntegerRange { field, min, max } | Self::DatetimeRange { field, min, max } => {
                if field.is_empty() || field.len() > MAX_CLAUSE_VALUE_LEN || min > max {
                    return Err(QdrantBackendError::contract(
                        QdrantBackendDiagnosticCode::InvalidFilterContract,
                    ));
                }
            }
        }
        Ok(())
    }

    /// Payload field that must be indexed for this clause.
    pub fn required_field(&self) -> &str {
        match self {
            Self::Source { .. } => "source",
            Self::Collection { .. } => "collection",
            Self::Tenant { .. } => "tenant",
            Self::Acl { .. } => "acl",
            Self::Lifecycle { .. } => "lifecycle",
            Self::IntegerRange { field, .. } | Self::DatetimeRange { field, .. } => field.as_str(),
        }
    }

    /// Payload index kind required for native evaluation of this clause.
    pub const fn required_index_kind(&self) -> PayloadIndexKind {
        match self {
            Self::Source { .. } | Self::Collection { .. } | Self::Tenant { .. } => {
                PayloadIndexKind::Keyword
            }
            Self::Acl { .. } => PayloadIndexKind::Acl,
            Self::Lifecycle { .. } => PayloadIndexKind::Lifecycle,
            Self::IntegerRange { .. } => PayloadIndexKind::IntegerRange,
            Self::DatetimeRange { .. } => PayloadIndexKind::DatetimeRange,
        }
    }
}

/// Validated multi-dimensional filter contract for one Qdrant search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QdrantFilterContract {
    strictness: FilterStrictness,
    clauses: Vec<FilterClause>,
    payload_indexes: PayloadIndexPlan,
    /// True only when every clause is backed by a matching payload index.
    native_support: bool,
}

impl<'de> Deserialize<'de> for QdrantFilterContract {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            strictness: FilterStrictness,
            clauses: Vec<FilterClause>,
            payload_indexes: PayloadIndexPlan,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.strictness, wire.clauses, wire.payload_indexes)
            .map_err(serde::de::Error::custom)
    }
}

impl QdrantFilterContract {
    /// Builds a filter contract.
    ///
    /// Strict mode fails closed unless every clause has a matching payload index
    /// (field + kind). `native_support` is derived from that binding and is not
    /// caller-asserted.
    pub fn new(
        strictness: FilterStrictness,
        clauses: Vec<FilterClause>,
        payload_indexes: PayloadIndexPlan,
    ) -> QdrantBackendResult<Self> {
        if clauses.len() > MAX_FILTER_CLAUSES {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidFilterContract,
            ));
        }
        for clause in &clauses {
            clause.validate()?;
        }

        let mut all_native = true;
        for clause in &clauses {
            if !payload_indexes.covers(clause.required_field(), clause.required_index_kind()) {
                all_native = false;
                if strictness == FilterStrictness::StrictNativeOrFailClosed {
                    return Err(QdrantBackendError::contract(
                        QdrantBackendDiagnosticCode::StrictFilterUnsupported,
                    ));
                }
            }
        }

        Ok(Self {
            strictness,
            clauses,
            payload_indexes,
            native_support: all_native,
        })
    }

    pub const fn strictness(&self) -> FilterStrictness {
        self.strictness
    }

    pub fn clauses(&self) -> &[FilterClause] {
        &self.clauses
    }

    pub const fn payload_indexes(&self) -> &PayloadIndexPlan {
        &self.payload_indexes
    }

    pub const fn native_support(&self) -> bool {
        self.native_support
    }
}
