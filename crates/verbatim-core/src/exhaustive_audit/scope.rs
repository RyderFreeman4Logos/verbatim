//! Declared audit scope and prerequisite freshness/index checks.

use serde::{Deserialize, Serialize};

use super::error::{ExhaustiveAuditError, ExhaustiveAuditResult};
use super::util::require_non_empty;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeFreshness {
    CheckedFresh,
    Stale,
    Unknown,
}

impl ScopeFreshness {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::CheckedFresh => "checked_fresh",
            Self::Stale => "stale",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeIndexCoverage {
    Complete,
    Partial,
    Unsupported,
    Unknown,
}

impl ScopeIndexCoverage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Partial => "partial",
            Self::Unsupported => "unsupported",
            Self::Unknown => "unknown",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AuditScopeMember {
    pub collection_id: String,
    pub source_id: String,
    pub snapshot_id: String,
    pub freshness: ScopeFreshness,
    pub index_coverage: ScopeIndexCoverage,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuditScopeMemberFields {
    pub collection_id: String,
    pub source_id: String,
    pub snapshot_id: String,
    pub freshness: ScopeFreshness,
    pub index_coverage: ScopeIndexCoverage,
}

impl AuditScopeMember {
    pub fn new(fields: AuditScopeMemberFields) -> ExhaustiveAuditResult<Self> {
        let member = Self {
            collection_id: fields.collection_id,
            source_id: fields.source_id,
            snapshot_id: fields.snapshot_id,
            freshness: fields.freshness,
            index_coverage: fields.index_coverage,
        };
        member.validate()?;
        Ok(member)
    }

    pub fn key(&self) -> String {
        format!(
            "{}:{}:{}",
            self.collection_id, self.source_id, self.snapshot_id
        )
    }

    pub fn validate(&self) -> ExhaustiveAuditResult<()> {
        require_non_empty("collection_id", &self.collection_id)?;
        require_non_empty("source_id", &self.source_id)?;
        require_non_empty("snapshot_id", &self.snapshot_id)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DeclaredAuditScope {
    pub scope_id: String,
    pub members: Vec<AuditScopeMember>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredAuditScopeFields {
    pub scope_id: String,
    pub members: Vec<AuditScopeMember>,
}

impl DeclaredAuditScope {
    pub fn new(fields: DeclaredAuditScopeFields) -> ExhaustiveAuditResult<Self> {
        let scope = Self {
            scope_id: fields.scope_id,
            members: fields.members,
        };
        scope.validate()?;
        Ok(scope)
    }

    pub fn validate(&self) -> ExhaustiveAuditResult<()> {
        require_non_empty("scope_id", &self.scope_id)?;
        if self.members.is_empty() {
            return Err(ExhaustiveAuditError::validation(
                "declared scope requires members",
            ));
        }
        let mut keys = std::collections::BTreeSet::new();
        for member in &self.members {
            member.validate()?;
            if !keys.insert(member.key()) {
                return Err(ExhaustiveAuditError::validation(
                    "declared scope must not contain duplicate collection/source/snapshot members",
                ));
            }
        }
        Ok(())
    }

    /// Refuse deterministic claims until every declared member is fresh and fully indexed.
    pub fn require_deterministic_coverage(&self) -> ExhaustiveAuditResult<()> {
        self.validate()?;
        for member in &self.members {
            if member.freshness != ScopeFreshness::CheckedFresh {
                return Err(ExhaustiveAuditError::ScopeUnavailable {
                    detail: format!(
                        "{} freshness is {}",
                        member.key(),
                        member.freshness.as_str()
                    ),
                });
            }
            if member.index_coverage != ScopeIndexCoverage::Complete {
                return Err(ExhaustiveAuditError::ScopeUnavailable {
                    detail: format!(
                        "{} index coverage is {}",
                        member.key(),
                        member.index_coverage.as_str()
                    ),
                });
            }
        }
        Ok(())
    }
}
