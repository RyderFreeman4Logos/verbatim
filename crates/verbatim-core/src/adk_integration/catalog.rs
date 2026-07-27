//! Catalog of the selected ADK-Rust crates and their non-negotiable boundaries.

use serde::{Deserialize, Serialize};

use crate::adk_integration::{
    AdkIntegrationDiagnosticCode, AdkIntegrationError, AdkIntegrationResult,
};

/// The selected public ADK-Rust crates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub enum AdkCrateName {
    #[serde(rename = "adk-core")]
    Core,
    #[serde(rename = "adk-agent")]
    Agent,
    #[serde(rename = "adk-model")]
    Model,
    #[serde(rename = "adk-tool")]
    Tool,
    #[serde(rename = "adk-runner")]
    Runner,
    #[serde(rename = "adk-graph")]
    Graph,
    #[serde(rename = "adk-session")]
    Session,
    #[serde(rename = "adk-artifact")]
    Artifact,
    #[serde(rename = "adk-auth")]
    Auth,
    #[serde(rename = "adk-telemetry")]
    Telemetry,
    #[serde(rename = "adk-guardrail")]
    Guardrail,
    #[serde(rename = "adk-eval")]
    Eval,
    #[serde(rename = "adk-rag")]
    Rag,
    #[serde(rename = "adk-memory")]
    Memory,
    #[serde(rename = "adk-server")]
    Server,
    #[serde(rename = "adk-action")]
    Action,
    #[serde(rename = "adk-sandbox")]
    Sandbox,
    #[serde(rename = "adk-mistralrs")]
    Mistralrs,
}

impl AdkCrateName {
    pub const ALL: [Self; 18] = [
        Self::Core,
        Self::Agent,
        Self::Model,
        Self::Tool,
        Self::Runner,
        Self::Graph,
        Self::Session,
        Self::Artifact,
        Self::Auth,
        Self::Telemetry,
        Self::Guardrail,
        Self::Eval,
        Self::Rag,
        Self::Memory,
        Self::Server,
        Self::Action,
        Self::Sandbox,
        Self::Mistralrs,
    ];

    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Core => "adk-core",
            Self::Agent => "adk-agent",
            Self::Model => "adk-model",
            Self::Tool => "adk-tool",
            Self::Runner => "adk-runner",
            Self::Graph => "adk-graph",
            Self::Session => "adk-session",
            Self::Artifact => "adk-artifact",
            Self::Auth => "adk-auth",
            Self::Telemetry => "adk-telemetry",
            Self::Guardrail => "adk-guardrail",
            Self::Eval => "adk-eval",
            Self::Rag => "adk-rag",
            Self::Memory => "adk-memory",
            Self::Server => "adk-server",
            Self::Action => "adk-action",
            Self::Sandbox => "adk-sandbox",
            Self::Mistralrs => "adk-mistralrs",
        }
    }

    const fn default_disposition(self) -> AdkCrateDisposition {
        match self {
            Self::Core
            | Self::Agent
            | Self::Tool
            | Self::Runner
            | Self::Graph
            | Self::Guardrail
            | Self::Eval => AdkCrateDisposition::Adopt,
            Self::Model
            | Self::Session
            | Self::Artifact
            | Self::Auth
            | Self::Telemetry
            | Self::Rag
            | Self::Action => AdkCrateDisposition::Wrap,
            Self::Sandbox => AdkCrateDisposition::Upstream,
            Self::Memory | Self::Server | Self::Mistralrs => AdkCrateDisposition::Keep,
        }
    }

    const fn default_constraints(self) -> &'static [AdkBoundaryConstraint] {
        match self {
            Self::Core => &[AdkBoundaryConstraint::CoreAbstractionsOnly],
            Self::Agent => &[AdkBoundaryConstraint::AgentImplementationsOnly],
            Self::Model => &[AdkBoundaryConstraint::EndpointProfilesOnly],
            Self::Tool => &[AdkBoundaryConstraint::ToolSchemasAndScopesOnly],
            Self::Runner => &[AdkBoundaryConstraint::ExecutionEventRuntimeOnly],
            Self::Graph => &[AdkBoundaryConstraint::BoundedWorkflowDagOnly],
            Self::Session => &[
                AdkBoundaryConstraint::WorkflowSessionOnly,
                AdkBoundaryConstraint::NotVerbatimSession,
            ],
            Self::Artifact => &[
                AdkBoundaryConstraint::WorkflowArtifactsOnly,
                AdkBoundaryConstraint::NotEvidenceStore,
            ],
            Self::Auth => &[
                AdkBoundaryConstraint::OidcJwtRbacPlumbingOnly,
                AdkBoundaryConstraint::NotDataPlaneAcl,
            ],
            Self::Telemetry => &[AdkBoundaryConstraint::VerbatimSpansOnly],
            Self::Guardrail => &[AdkBoundaryConstraint::SupplementalOnly],
            Self::Eval => &[AdkBoundaryConstraint::WorkflowEvaluationOnly],
            Self::Rag => &[
                AdkBoundaryConstraint::GenericProvidersOnly,
                AdkBoundaryConstraint::NotSourceTruth,
            ],
            Self::Memory => &[
                AdkBoundaryConstraint::AgentMemoryOnly,
                AdkBoundaryConstraint::NotEvidenceStore,
            ],
            Self::Server => &[
                AdkBoundaryConstraint::OptionalSidecarOnly,
                AdkBoundaryConstraint::NotCanonicalDaemon,
            ],
            Self::Action => &[AdkBoundaryConstraint::CapabilityWhitelistedOnly],
            Self::Sandbox => &[AdkBoundaryConstraint::PlatformSecurityConformanceRequired],
            Self::Mistralrs => &[AdkBoundaryConstraint::OptionalAdapterOnly],
        }
    }
}

/// The #364 adoption disposition for an ADK-Rust crate.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdkCrateDisposition {
    Adopt,
    Wrap,
    Upstream,
    Keep,
    Delete,
}

/// A constraint preventing an adopted capability from becoming a Verbatim domain type.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdkBoundaryConstraint {
    CoreAbstractionsOnly,
    AgentImplementationsOnly,
    EndpointProfilesOnly,
    ToolSchemasAndScopesOnly,
    ExecutionEventRuntimeOnly,
    BoundedWorkflowDagOnly,
    WorkflowSessionOnly,
    NotVerbatimSession,
    WorkflowArtifactsOnly,
    NotEvidenceStore,
    OidcJwtRbacPlumbingOnly,
    NotDataPlaneAcl,
    VerbatimSpansOnly,
    SupplementalOnly,
    WorkflowEvaluationOnly,
    GenericProvidersOnly,
    NotSourceTruth,
    AgentMemoryOnly,
    OptionalSidecarOnly,
    NotCanonicalDaemon,
    CapabilityWhitelistedOnly,
    PlatformSecurityConformanceRequired,
    OptionalAdapterOnly,
}

/// Platform-security evidence state for the sandbox adoption exception.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PlatformSecurityConformance {
    NotRequired,
    Pending,
    Verified,
}

/// The decision and constraints for one ADK-Rust crate.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdkCratePolicy {
    crate_name: AdkCrateName,
    disposition: AdkCrateDisposition,
    constraints: Vec<AdkBoundaryConstraint>,
    security_conformance: PlatformSecurityConformance,
}

impl AdkCratePolicy {
    fn standard_for(crate_name: AdkCrateName) -> Self {
        let security_conformance = match crate_name {
            AdkCrateName::Sandbox => PlatformSecurityConformance::Pending,
            _ => PlatformSecurityConformance::NotRequired,
        };
        Self {
            crate_name,
            disposition: crate_name.default_disposition(),
            constraints: crate_name.default_constraints().to_vec(),
            security_conformance,
        }
    }

    pub const fn crate_name(&self) -> AdkCrateName {
        self.crate_name
    }

    pub const fn disposition(&self) -> AdkCrateDisposition {
        self.disposition
    }

    pub fn constraints(&self) -> &[AdkBoundaryConstraint] {
        &self.constraints
    }

    pub const fn security_conformance(&self) -> PlatformSecurityConformance {
        self.security_conformance
    }
}

/// Complete selected-crate catalog for the optional ADK-Rust integration.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AdkCrateCatalog {
    entries: Vec<AdkCratePolicy>,
}

impl AdkCrateCatalog {
    /// Returns the default policy catalog mandated by issue #365.
    pub fn standard() -> Self {
        Self {
            entries: AdkCrateName::ALL
                .iter()
                .copied()
                .map(AdkCratePolicy::standard_for)
                .collect(),
        }
    }

    pub fn entries(&self) -> &[AdkCratePolicy] {
        &self.entries
    }

    pub fn entry(&self, crate_name: AdkCrateName) -> Option<&AdkCratePolicy> {
        self.entries
            .iter()
            .find(|entry| entry.crate_name == crate_name)
    }

    /// Validates full catalog coverage, policy dispositions, and all constraints.
    pub fn validate(&self) -> AdkIntegrationResult<()> {
        if self.entries.len() != AdkCrateName::ALL.len() {
            return Err(AdkIntegrationError::validation(
                AdkIntegrationDiagnosticCode::CatalogCoverageInvalid,
            ));
        }

        for crate_name in AdkCrateName::ALL {
            let matching_entries = self
                .entries
                .iter()
                .filter(|entry| entry.crate_name == crate_name)
                .count();
            if matching_entries != 1 {
                return Err(AdkIntegrationError::validation(
                    AdkIntegrationDiagnosticCode::CatalogCoverageInvalid,
                ));
            }

            let Some(entry) = self.entry(crate_name) else {
                return Err(AdkIntegrationError::validation(
                    AdkIntegrationDiagnosticCode::CatalogCoverageInvalid,
                ));
            };
            if entry.constraints.as_slice() != crate_name.default_constraints() {
                return Err(AdkIntegrationError::validation(
                    AdkIntegrationDiagnosticCode::CatalogConstraintsInvalid,
                ));
            }
            Self::validate_entry_disposition(entry)?;
        }
        Ok(())
    }

    fn validate_entry_disposition(entry: &AdkCratePolicy) -> AdkIntegrationResult<()> {
        match entry.crate_name {
            AdkCrateName::Sandbox => {
                if entry.disposition != AdkCrateDisposition::Upstream
                    && entry.disposition != AdkCrateDisposition::Adopt
                {
                    return Err(AdkIntegrationError::validation(
                        AdkIntegrationDiagnosticCode::CatalogDispositionInvalid,
                    ));
                }
                if entry.disposition == AdkCrateDisposition::Adopt
                    && entry.security_conformance != PlatformSecurityConformance::Verified
                {
                    return Err(AdkIntegrationError::validation(
                        AdkIntegrationDiagnosticCode::SandboxSecurityConformanceRequired,
                    ));
                }
            }
            _ => {
                if entry.disposition != entry.crate_name.default_disposition() {
                    return Err(AdkIntegrationError::validation(
                        AdkIntegrationDiagnosticCode::CatalogDispositionInvalid,
                    ));
                }
                if entry.security_conformance != PlatformSecurityConformance::NotRequired {
                    return Err(AdkIntegrationError::validation(
                        AdkIntegrationDiagnosticCode::CatalogConstraintsInvalid,
                    ));
                }
            }
        }
        Ok(())
    }
}

/// Encodes a validated catalog without exposing serialization errors or input.
pub fn encode_adk_crate_catalog_json(catalog: &AdkCrateCatalog) -> AdkIntegrationResult<String> {
    catalog.validate()?;
    serde_json::to_string(catalog).map_err(|_| {
        AdkIntegrationError::validation(AdkIntegrationDiagnosticCode::CatalogSerializationFailed)
    })
}

/// Decodes and validates a catalog at the serialization boundary.
pub fn decode_adk_crate_catalog_json(input: &str) -> AdkIntegrationResult<AdkCrateCatalog> {
    let catalog: AdkCrateCatalog = serde_json::from_str(input).map_err(|_| {
        AdkIntegrationError::validation(AdkIntegrationDiagnosticCode::InvalidCatalogJson)
    })?;
    catalog.validate()?;
    Ok(catalog)
}
