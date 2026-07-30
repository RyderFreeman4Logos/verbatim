//! Multi-source / collection / tenant / ACL filter contract for Qdrant.

use serde::{Deserialize, Serialize};

use super::{
    PayloadIndexPlan, QdrantBackendDiagnosticCode, QdrantBackendError, QdrantBackendResult,
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
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterClause {
    Source { value: String },
    Collection { value: String },
    Tenant { value: String },
    Acl { value: String },
    Lifecycle { value: String },
}

impl FilterClause {
    fn validate(&self) -> QdrantBackendResult<()> {
        let value = match self {
            Self::Source { value }
            | Self::Collection { value }
            | Self::Tenant { value }
            | Self::Acl { value }
            | Self::Lifecycle { value } => value,
        };
        if value.is_empty() || value.len() > MAX_CLAUSE_VALUE_LEN {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidFilterContract,
            ));
        }
        Ok(())
    }

    fn requires_native_support(&self) -> bool {
        matches!(
            self,
            Self::Source { .. }
                | Self::Collection { .. }
                | Self::Tenant { .. }
                | Self::Acl { .. }
                | Self::Lifecycle { .. }
        )
    }
}

/// Validated multi-dimensional filter contract for one Qdrant search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct QdrantFilterContract {
    strictness: FilterStrictness,
    clauses: Vec<FilterClause>,
    payload_indexes: PayloadIndexPlan,
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
            native_support: bool,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.strictness,
            wire.clauses,
            wire.payload_indexes,
            wire.native_support,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl QdrantFilterContract {
    /// Builds a filter contract. Strict unsupported filters fail closed.
    pub fn new(
        strictness: FilterStrictness,
        clauses: Vec<FilterClause>,
        payload_indexes: PayloadIndexPlan,
        native_support: bool,
    ) -> QdrantBackendResult<Self> {
        if clauses.len() > MAX_FILTER_CLAUSES {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidFilterContract,
            ));
        }
        for clause in &clauses {
            clause.validate()?;
        }
        if strictness == FilterStrictness::StrictNativeOrFailClosed
            && !native_support
            && clauses.iter().any(FilterClause::requires_native_support)
        {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::StrictFilterUnsupported,
            ));
        }
        Ok(Self {
            strictness,
            clauses,
            payload_indexes,
            native_support,
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
