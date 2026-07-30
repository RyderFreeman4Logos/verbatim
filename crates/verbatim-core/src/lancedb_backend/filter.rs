//! Scalar prefilter contract: every strict predicate has a typed scalar-index binding.

use serde::{Deserialize, Serialize};

use super::{LanceDbBackendDiagnosticCode, LanceDbBackendError, LanceDbBackendResult};

const MAX_INDEXES: usize = 32;
const MAX_FIELD_LEN: usize = 64;
const MAX_FILTER_CLAUSES: usize = 32;
const MAX_VALUE_LEN: usize = 256;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LanceDbScalarIndexKind {
    BTree,
    Bitmap,
    LabelList,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanceDbScalarIndexRequirement {
    field: String,
    kind: LanceDbScalarIndexKind,
}

impl<'de> Deserialize<'de> for LanceDbScalarIndexRequirement {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            field: String,
            kind: LanceDbScalarIndexKind,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.field, wire.kind).map_err(serde::de::Error::custom)
    }
}

impl LanceDbScalarIndexRequirement {
    pub fn new(
        field: impl Into<String>,
        kind: LanceDbScalarIndexKind,
    ) -> LanceDbBackendResult<Self> {
        let field = field.into();
        if !is_field_name(&field) {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::InvalidScalarIndexPlan,
            ));
        }
        Ok(Self { field, kind })
    }

    pub fn field(&self) -> &str {
        &self.field
    }

    pub const fn kind(&self) -> LanceDbScalarIndexKind {
        self.kind
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanceDbScalarIndexPlan {
    requirements: Vec<LanceDbScalarIndexRequirement>,
}

impl<'de> Deserialize<'de> for LanceDbScalarIndexPlan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            requirements: Vec<LanceDbScalarIndexRequirement>,
        }
        Self::new(Wire::deserialize(deserializer)?.requirements).map_err(serde::de::Error::custom)
    }
}

impl LanceDbScalarIndexPlan {
    pub fn new(requirements: Vec<LanceDbScalarIndexRequirement>) -> LanceDbBackendResult<Self> {
        if requirements.is_empty() || requirements.len() > MAX_INDEXES {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::InvalidScalarIndexPlan,
            ));
        }
        let mut seen = std::collections::BTreeSet::new();
        if requirements
            .iter()
            .any(|requirement| !seen.insert(requirement.field.clone()))
        {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::InvalidScalarIndexPlan,
            ));
        }
        Ok(Self { requirements })
    }

    pub fn covers(&self, field: &str, kind: LanceDbScalarIndexKind) -> bool {
        self.requirements
            .iter()
            .any(|requirement| requirement.field == field && requirement.kind == kind)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterStrictness {
    StrictNativeOrFailClosed,
    BestEffort,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FilterClause {
    Source { value: String },
    Collection { value: String },
    Tenant { value: String },
    Acl { value: String },
    Lifecycle { value: String },
    TimestampMicros { min: i64, max: i64 },
}

impl FilterClause {
    fn validate(&self) -> LanceDbBackendResult<()> {
        match self {
            Self::Source { value }
            | Self::Collection { value }
            | Self::Tenant { value }
            | Self::Acl { value }
            | Self::Lifecycle { value }
                if value.is_empty() || value.len() > MAX_VALUE_LEN =>
            {
                Err(LanceDbBackendError::contract(
                    LanceDbBackendDiagnosticCode::InvalidFilterContract,
                ))
            }
            Self::TimestampMicros { min, max } if min > max => Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::InvalidFilterContract,
            )),
            _ => Ok(()),
        }
    }

    const fn required_binding(&self) -> (&str, LanceDbScalarIndexKind) {
        match self {
            Self::Source { .. } => ("source", LanceDbScalarIndexKind::BTree),
            Self::Collection { .. } => ("collection", LanceDbScalarIndexKind::BTree),
            Self::Tenant { .. } => ("tenant", LanceDbScalarIndexKind::BTree),
            Self::Acl { .. } => ("acl", LanceDbScalarIndexKind::LabelList),
            Self::Lifecycle { .. } => ("lifecycle", LanceDbScalarIndexKind::Bitmap),
            Self::TimestampMicros { .. } => ("timestamp_micros", LanceDbScalarIndexKind::BTree),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct LanceDbFilterContract {
    strictness: FilterStrictness,
    clauses: Vec<FilterClause>,
    scalar_indexes: LanceDbScalarIndexPlan,
    selectivity_ppm: u32,
    native_support: bool,
}

impl<'de> Deserialize<'de> for LanceDbFilterContract {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            strictness: FilterStrictness,
            clauses: Vec<FilterClause>,
            scalar_indexes: LanceDbScalarIndexPlan,
            selectivity_ppm: u32,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(
            wire.strictness,
            wire.clauses,
            wire.scalar_indexes,
            wire.selectivity_ppm,
        )
        .map_err(serde::de::Error::custom)
    }
}

impl LanceDbFilterContract {
    pub fn new(
        strictness: FilterStrictness,
        clauses: Vec<FilterClause>,
        scalar_indexes: LanceDbScalarIndexPlan,
        selectivity_ppm: u32,
    ) -> LanceDbBackendResult<Self> {
        if clauses.is_empty() || clauses.len() > MAX_FILTER_CLAUSES || selectivity_ppm > 1_000_000 {
            return Err(LanceDbBackendError::contract(
                LanceDbBackendDiagnosticCode::InvalidFilterContract,
            ));
        }
        let mut native_support = true;
        for clause in &clauses {
            clause.validate()?;
            let (field, kind) = clause.required_binding();
            if !scalar_indexes.covers(field, kind) {
                native_support = false;
                if strictness == FilterStrictness::StrictNativeOrFailClosed {
                    return Err(LanceDbBackendError::contract(
                        LanceDbBackendDiagnosticCode::StrictFilterUnbound,
                    ));
                }
            }
        }
        Ok(Self {
            strictness,
            clauses,
            scalar_indexes,
            selectivity_ppm,
            native_support,
        })
    }

    pub const fn strictness(&self) -> FilterStrictness {
        self.strictness
    }

    pub fn clauses(&self) -> &[FilterClause] {
        &self.clauses
    }

    pub const fn selectivity_ppm(&self) -> u32 {
        self.selectivity_ppm
    }

    pub const fn native_support(&self) -> bool {
        self.native_support
    }
}

fn is_field_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_FIELD_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_'))
}
