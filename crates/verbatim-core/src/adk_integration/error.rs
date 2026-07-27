//! Closed diagnostic-only failures for the ADK-Rust integration contract.

use std::error::Error;
use std::fmt;

use serde::{Deserialize, Serialize};

pub type AdkIntegrationResult<T> = Result<T, AdkIntegrationError>;

/// Closed diagnostic codes; errors never retain caller-controlled input.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdkIntegrationDiagnosticCode {
    CatalogCoverageInvalid,
    CatalogDispositionInvalid,
    CatalogConstraintsInvalid,
    SandboxSecurityConformanceRequired,
    ArtifactSchemaBoundaryForbidden,
    WireSchemaBoundaryForbidden,
    WorkflowStorageBoundaryForbidden,
    ScopeAclBoundaryForbidden,
    GraphKnowledgeBoundaryForbidden,
    VersionMustBeExactStableOneX,
    CatalogSerializationFailed,
    InvalidCatalogJson,
    VersionPolicySerializationFailed,
    InvalidVersionPolicyJson,
}

impl AdkIntegrationDiagnosticCode {
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::CatalogCoverageInvalid => "catalog_coverage_invalid",
            Self::CatalogDispositionInvalid => "catalog_disposition_invalid",
            Self::CatalogConstraintsInvalid => "catalog_constraints_invalid",
            Self::SandboxSecurityConformanceRequired => "sandbox_security_conformance_required",
            Self::ArtifactSchemaBoundaryForbidden => "artifact_schema_boundary_forbidden",
            Self::WireSchemaBoundaryForbidden => "wire_schema_boundary_forbidden",
            Self::WorkflowStorageBoundaryForbidden => "workflow_storage_boundary_forbidden",
            Self::ScopeAclBoundaryForbidden => "scope_acl_boundary_forbidden",
            Self::GraphKnowledgeBoundaryForbidden => "graph_knowledge_boundary_forbidden",
            Self::VersionMustBeExactStableOneX => "version_must_be_exact_stable_one_x",
            Self::CatalogSerializationFailed => "catalog_serialization_failed",
            Self::InvalidCatalogJson => "invalid_catalog_json",
            Self::VersionPolicySerializationFailed => "version_policy_serialization_failed",
            Self::InvalidVersionPolicyJson => "invalid_version_policy_json",
        }
    }
}

/// An ADK integration failure contains only a closed diagnostic code.
#[derive(Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "class", rename_all = "snake_case")]
pub enum AdkIntegrationError {
    Validation { code: AdkIntegrationDiagnosticCode },
}

impl AdkIntegrationError {
    pub const fn validation(code: AdkIntegrationDiagnosticCode) -> Self {
        Self::Validation { code }
    }

    pub const fn diagnostic_code(self) -> AdkIntegrationDiagnosticCode {
        match self {
            Self::Validation { code } => code,
        }
    }
}

impl fmt::Debug for AdkIntegrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "AdkIntegrationError({})",
            self.diagnostic_code().as_str()
        )
    }
}

impl fmt::Display for AdkIntegrationError {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            formatter,
            "adk-integration.{}",
            self.diagnostic_code().as_str()
        )
    }
}

impl Error for AdkIntegrationError {}
