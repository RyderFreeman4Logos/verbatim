//! Two-sided source/version identity and comparison constraints.

use serde::{Deserialize, Serialize};

use super::error::{ComparisonError, ComparisonResultType};
use super::util::{require_non_empty, require_unique_non_empty};

/// Lifecycle state declared for the source version selected into a scope.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceLifecycle {
    Active,
    Superseded,
    Retired,
    Archived,
}

impl SourceLifecycle {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Active => "active",
            Self::Superseded => "superseded",
            Self::Retired => "retired",
            Self::Archived => "archived",
        }
    }
}

/// Availability and ACL resolution state for a requested version.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceAvailability {
    Authorized,
    AclDenied,
    VersionGone,
    Unresolved,
}

impl SourceAvailability {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Authorized => "authorized",
            Self::AclDenied => "acl_denied",
            Self::VersionGone => "version_gone",
            Self::Unresolved => "unresolved",
        }
    }
}

/// One immutable source/version identity with comparison-relevant constraints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceVersion {
    pub source_id: String,
    pub version_id: String,
    pub lifecycle: SourceLifecycle,
    /// ISO-like effective-date label supplied by the source catalogue.
    pub effective_date: Option<String>,
    pub jurisdictions: Vec<String>,
    pub products: Vec<String>,
    pub availability: SourceAvailability,
}

/// Construction fields for [`SourceVersion`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceVersionFields {
    pub source_id: String,
    pub version_id: String,
    pub lifecycle: SourceLifecycle,
    pub effective_date: Option<String>,
    pub jurisdictions: Vec<String>,
    pub products: Vec<String>,
    pub availability: SourceAvailability,
}

impl SourceVersion {
    pub fn new(fields: SourceVersionFields) -> ComparisonResultType<Self> {
        let source = Self {
            source_id: fields.source_id,
            version_id: fields.version_id,
            lifecycle: fields.lifecycle,
            effective_date: fields.effective_date,
            jurisdictions: fields.jurisdictions,
            products: fields.products,
            availability: fields.availability,
        };
        source.validate_identity()?;
        Ok(source)
    }

    pub fn validate_identity(&self) -> ComparisonResultType<()> {
        require_non_empty("source_id", &self.source_id)?;
        require_non_empty("version_id", &self.version_id)?;
        if let Some(effective_date) = &self.effective_date {
            require_non_empty("effective_date", effective_date)?;
        }
        require_unique_non_empty("jurisdiction", &self.jurisdictions)?;
        require_unique_non_empty("product", &self.products)?;
        Ok(())
    }

    /// Reject a lifecycle/ACL state that cannot safely provide source evidence.
    pub fn require_resolved_authorization(&self) -> ComparisonResultType<()> {
        self.validate_identity()?;
        match self.availability {
            SourceAvailability::Authorized => Ok(()),
            SourceAvailability::AclDenied => Err(ComparisonError::AclDenied {
                source_id: self.source_id.clone(),
                version_id: self.version_id.clone(),
            }),
            SourceAvailability::VersionGone => Err(ComparisonError::VersionGone {
                source_id: self.source_id.clone(),
                version_id: self.version_id.clone(),
            }),
            SourceAvailability::Unresolved => Err(ComparisonError::scope_unresolved(format!(
                "availability unresolved for {}@{}",
                self.source_id, self.version_id
            ))),
        }
    }
}

/// Exactly two source/version identities and their declared constraints.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ComparisonScope {
    pub scope_id: String,
    pub left: SourceVersion,
    pub right: SourceVersion,
    /// Optional user question that motivates dimension decomposition.
    pub comparison_question: Option<String>,
}

/// Construction fields for [`ComparisonScope`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComparisonScopeFields {
    pub scope_id: String,
    pub left: SourceVersion,
    pub right: SourceVersion,
    pub comparison_question: Option<String>,
}

impl ComparisonScope {
    pub fn new(fields: ComparisonScopeFields) -> ComparisonResultType<Self> {
        let scope = Self {
            scope_id: fields.scope_id,
            left: fields.left,
            right: fields.right,
            comparison_question: fields.comparison_question,
        };
        scope.validate()?;
        Ok(scope)
    }

    /// Structural validation deliberately permits unresolved sources so an
    /// adapter can project resolution failures to their typed error variants.
    pub fn validate(&self) -> ComparisonResultType<()> {
        require_non_empty("scope_id", &self.scope_id)?;
        self.left.validate_identity()?;
        self.right.validate_identity()?;
        if self.left.source_id == self.right.source_id
            && self.left.version_id == self.right.version_id
        {
            return Err(ComparisonError::validation(
                "comparison scope requires two distinct source/version identities",
            ));
        }
        if let Some(question) = &self.comparison_question {
            require_non_empty("comparison_question", question)?;
        }
        Ok(())
    }

    /// Fail closed before extract/align: both sides must be authorized and
    /// share an active lifecycle and declared comparison constraints.
    pub fn require_comparable(&self) -> ComparisonResultType<()> {
        self.validate()?;
        self.left.require_resolved_authorization()?;
        self.right.require_resolved_authorization()?;
        for source in [&self.left, &self.right] {
            if source.lifecycle != SourceLifecycle::Active {
                return Err(ComparisonError::scope_unresolved(format!(
                    "lifecycle {} is not comparable for {}@{}",
                    source.lifecycle.as_str(),
                    source.source_id,
                    source.version_id
                )));
            }
        }
        if self.left.effective_date != self.right.effective_date {
            return Err(ComparisonError::scope_unresolved(
                "source effective-date constraints do not match",
            ));
        }
        require_shared_constraint(
            "jurisdiction",
            &self.left.jurisdictions,
            &self.right.jurisdictions,
        )?;
        require_shared_constraint("product", &self.left.products, &self.right.products)
    }

    pub fn source_count(&self) -> u32 {
        2
    }
}

fn require_shared_constraint(
    field: &str,
    left: &[String],
    right: &[String],
) -> ComparisonResultType<()> {
    if left.is_empty() || right.is_empty() || !left.iter().any(|value| right.contains(value)) {
        return Err(ComparisonError::scope_unresolved(format!(
            "source {field} constraints have no shared value"
        )));
    }
    Ok(())
}
