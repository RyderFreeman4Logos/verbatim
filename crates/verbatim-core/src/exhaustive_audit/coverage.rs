//! Per-member coverage manifest and completeness projection.

use serde::{Deserialize, Serialize};

use super::enumeration::CandidateEnumeration;
use super::error::{ExhaustiveAuditError, ExhaustiveAuditResult};
use super::scope::DeclaredAuditScope;
use super::util::require_digest;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletenessTarget {
    All,
    Only,
    None,
    Every,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScopeCoverageStatus {
    Searched,
    Unsearched,
    Blocked,
    Stale,
    Unsupported,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CompletenessStatus {
    ExhaustiveOverDeclaredScope,
    Incomplete,
    UnableToEstablish,
    Blocked,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageEntry {
    pub member_key: String,
    pub status: ScopeCoverageStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CoverageManifest {
    pub scope_hash: String,
    pub entries: Vec<CoverageEntry>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageManifestFields {
    pub scope_hash: String,
    pub entries: Vec<CoverageEntry>,
}

impl CoverageManifest {
    pub fn new(fields: CoverageManifestFields) -> ExhaustiveAuditResult<Self> {
        let manifest = Self {
            scope_hash: fields.scope_hash,
            entries: fields.entries,
        };
        manifest.validate()?;
        Ok(manifest)
    }

    pub fn validate(&self) -> ExhaustiveAuditResult<()> {
        require_digest("coverage.scope_hash", &self.scope_hash)?;
        if self.entries.is_empty() {
            return Err(ExhaustiveAuditError::validation(
                "coverage manifest requires entries",
            ));
        }
        let mut keys = std::collections::BTreeSet::new();
        for entry in &self.entries {
            if entry.member_key.trim().is_empty() {
                return Err(ExhaustiveAuditError::validation(
                    "coverage member key must not be empty",
                ));
            }
            if !keys.insert(&entry.member_key) {
                return Err(ExhaustiveAuditError::validation(
                    "coverage manifest must not duplicate members",
                ));
            }
        }
        Ok(())
    }

    pub fn validate_for(
        &self,
        scope: &DeclaredAuditScope,
        scope_hash: &str,
    ) -> ExhaustiveAuditResult<()> {
        self.validate()?;
        if self.scope_hash != scope_hash {
            return Err(ExhaustiveAuditError::validation(
                "coverage manifest scope hash must match declared scope",
            ));
        }
        let expected: std::collections::BTreeSet<_> =
            scope.members.iter().map(|member| member.key()).collect();
        let actual: std::collections::BTreeSet<_> = self
            .entries
            .iter()
            .map(|entry| entry.member_key.clone())
            .collect();
        if actual != expected {
            return Err(ExhaustiveAuditError::validation(
                "coverage manifest must account for every declared scope member exactly once",
            ));
        }
        Ok(())
    }
}

/// Compute the strongest status justified by declared scope, coverage, and
/// enumerations. Approximate dense/graph/top-k passes are supplementary only.
pub fn establish_completeness(
    _target: CompletenessTarget,
    scope: &DeclaredAuditScope,
    manifest: &CoverageManifest,
    enumerations: &[CandidateEnumeration],
) -> ExhaustiveAuditResult<CompletenessStatus> {
    let scope_hash = super::content_hash_of(scope)?;
    manifest.validate_for(scope, &scope_hash)?;
    if manifest
        .entries
        .iter()
        .any(|entry| entry.status == ScopeCoverageStatus::Blocked)
    {
        return Ok(CompletenessStatus::Blocked);
    }
    if manifest
        .entries
        .iter()
        .any(|entry| entry.status != ScopeCoverageStatus::Searched)
    {
        return Ok(CompletenessStatus::Incomplete);
    }
    if scope.require_deterministic_coverage().is_err() {
        return Ok(CompletenessStatus::Incomplete);
    }
    let has_deterministic_primary = enumerations.iter().any(|enumeration| {
        enumeration.scope_hash == scope_hash && enumeration.is_deterministic_primary()
    });
    if !has_deterministic_primary {
        return Ok(CompletenessStatus::UnableToEstablish);
    }
    Ok(CompletenessStatus::ExhaustiveOverDeclaredScope)
}
