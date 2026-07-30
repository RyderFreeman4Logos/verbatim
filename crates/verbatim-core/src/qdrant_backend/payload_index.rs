//! Payload-index requirements for typed predicate plans.

use serde::{Deserialize, Serialize};

use super::{QdrantBackendDiagnosticCode, QdrantBackendError, QdrantBackendResult};

const MAX_PAYLOAD_FIELDS: usize = 32;
const MAX_FIELD_NAME_LEN: usize = 64;

/// Kind of payload index required by a typed predicate plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PayloadIndexKind {
    Keyword,
    IntegerRange,
    FloatRange,
    DatetimeRange,
    Acl,
    Lifecycle,
}

/// One required payload field index.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PayloadIndexRequirement {
    field: String,
    kind: PayloadIndexKind,
}

impl<'de> Deserialize<'de> for PayloadIndexRequirement {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            field: String,
            kind: PayloadIndexKind,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.field, wire.kind).map_err(serde::de::Error::custom)
    }
}

impl PayloadIndexRequirement {
    pub fn new(field: impl Into<String>, kind: PayloadIndexKind) -> QdrantBackendResult<Self> {
        let field = field.into();
        if !is_valid_field_name(&field) {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidPayloadIndexPlan,
            ));
        }
        Ok(Self { field, kind })
    }

    pub fn field(&self) -> &str {
        &self.field
    }

    pub const fn kind(&self) -> PayloadIndexKind {
        self.kind
    }
}

/// Closed set of payload indexes that must exist before strict filters run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PayloadIndexPlan {
    requirements: Vec<PayloadIndexRequirement>,
}

impl<'de> Deserialize<'de> for PayloadIndexPlan {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        #[derive(Deserialize)]
        struct Wire {
            requirements: Vec<PayloadIndexRequirement>,
        }
        let wire = Wire::deserialize(deserializer)?;
        Self::new(wire.requirements).map_err(serde::de::Error::custom)
    }
}

impl PayloadIndexPlan {
    /// Requires at least keyword, ACL, and lifecycle indexes for enterprise filters.
    pub fn new(requirements: Vec<PayloadIndexRequirement>) -> QdrantBackendResult<Self> {
        if requirements.is_empty() || requirements.len() > MAX_PAYLOAD_FIELDS {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidPayloadIndexPlan,
            ));
        }

        let mut has_keyword = false;
        let mut has_acl = false;
        let mut has_lifecycle = false;
        let mut seen = std::collections::BTreeSet::new();
        for requirement in &requirements {
            if !seen.insert(requirement.field().to_string()) {
                return Err(QdrantBackendError::contract(
                    QdrantBackendDiagnosticCode::InvalidPayloadIndexPlan,
                ));
            }
            match requirement.kind() {
                PayloadIndexKind::Keyword => has_keyword = true,
                PayloadIndexKind::Acl => has_acl = true,
                PayloadIndexKind::Lifecycle => has_lifecycle = true,
                PayloadIndexKind::IntegerRange
                | PayloadIndexKind::FloatRange
                | PayloadIndexKind::DatetimeRange => {}
            }
        }
        if !(has_keyword && has_acl && has_lifecycle) {
            return Err(QdrantBackendError::contract(
                QdrantBackendDiagnosticCode::InvalidPayloadIndexPlan,
            ));
        }
        Ok(Self { requirements })
    }

    pub fn requirements(&self) -> &[PayloadIndexRequirement] {
        &self.requirements
    }

    /// True when the plan contains an index for `field` with exactly `kind`.
    pub fn covers(&self, field: &str, kind: PayloadIndexKind) -> bool {
        self.requirements
            .iter()
            .any(|requirement| requirement.field() == field && requirement.kind() == kind)
    }
}

fn is_valid_field_name(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= MAX_FIELD_NAME_LEN
        && value.bytes().all(|byte| {
            byte.is_ascii_lowercase() || byte.is_ascii_digit() || matches!(byte, b'_' | b'.')
        })
}
