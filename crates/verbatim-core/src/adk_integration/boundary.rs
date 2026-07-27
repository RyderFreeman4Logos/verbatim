//! Explicit domain boundaries that ADK implementation types may not cross.

use serde::{Deserialize, Serialize};

use crate::adk_integration::AdkIntegrationDiagnosticCode;

/// A Verbatim domain boundary enforced before an ADK-backed adapter is accepted.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DomainBoundaryRule {
    /// Persisted Verbatim artifacts may not use ADK-only schemas.
    PersistedArtifacts,
    /// Public Verbatim wire APIs may not expose ADK-internal structs.
    PublicWireApi,
    /// Built-in workflows may not bypass Verbatim storage ports.
    BuiltInWorkflowStorage,
    /// Agent/tool scopes may not replace source or chunk ACL enforcement.
    AgentToolScopeAcl,
    /// An ADK workflow graph may not replace the GraphRAG knowledge graph.
    WorkflowGraphKnowledgeGraph,
}

impl DomainBoundaryRule {
    pub const ALL: [Self; 5] = [
        Self::PersistedArtifacts,
        Self::PublicWireApi,
        Self::BuiltInWorkflowStorage,
        Self::AgentToolScopeAcl,
        Self::WorkflowGraphKnowledgeGraph,
    ];

    pub const fn forbidden_use(self) -> AdkBoundaryUse {
        match self {
            Self::PersistedArtifacts => AdkBoundaryUse::AdkOnlyArtifactSchema,
            Self::PublicWireApi => AdkBoundaryUse::AdkInternalWireStruct,
            Self::BuiltInWorkflowStorage => AdkBoundaryUse::DirectStorageAccess,
            Self::AgentToolScopeAcl => AdkBoundaryUse::AgentToolScopeReplacingSourceChunkAcl,
            Self::WorkflowGraphKnowledgeGraph => {
                AdkBoundaryUse::AdkGraphReplacingGraphRagKnowledgeGraph
            }
        }
    }

    pub const fn diagnostic_code(self) -> AdkIntegrationDiagnosticCode {
        match self {
            Self::PersistedArtifacts => {
                AdkIntegrationDiagnosticCode::ArtifactSchemaBoundaryForbidden
            }
            Self::PublicWireApi => AdkIntegrationDiagnosticCode::WireSchemaBoundaryForbidden,
            Self::BuiltInWorkflowStorage => {
                AdkIntegrationDiagnosticCode::WorkflowStorageBoundaryForbidden
            }
            Self::AgentToolScopeAcl => AdkIntegrationDiagnosticCode::ScopeAclBoundaryForbidden,
            Self::WorkflowGraphKnowledgeGraph => {
                AdkIntegrationDiagnosticCode::GraphKnowledgeBoundaryForbidden
            }
        }
    }
}

/// The only ADK-related crossings this contract recognizes at its boundary.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AdkBoundaryUse {
    AdkOnlyArtifactSchema,
    AdkInternalWireStruct,
    DirectStorageAccess,
    AgentToolScopeReplacingSourceChunkAcl,
    AdkGraphReplacingGraphRagKnowledgeGraph,
    /// A stable Verbatim adapter converts between the two domains.
    StableVerbatimAdapter,
}
