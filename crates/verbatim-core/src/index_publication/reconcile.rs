//! Typed reconciliation findings across publication components.

use serde::{Deserialize, Serialize};

use crate::storage_ports::{StorageError, StorageGeneration, StorageResult};

use super::manifest::{ComponentKind, INDEX_PUBLICATION_SCHEMA_VERSION};

/// Severity of a reconciliation finding.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconciliationSeverity {
    Info,
    Warning,
    Error,
    Critical,
}

/// Machine-readable finding kind for divergent publication state.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReconciliationFindingKind {
    /// A component's observed generation diverges from the active manifest.
    DivergentComponent {
        component: ComponentKind,
        expected_generation: StorageGeneration,
        observed_generation: StorageGeneration,
    },
    /// A required chunk/artifact is missing under the bound generation.
    MissingChunk {
        chunk_id: String,
        generation: StorageGeneration,
    },
    /// Observed content hash does not match the declared digest.
    HashMismatch {
        component: ComponentKind,
        expected_digest: String,
        observed_digest: String,
    },
    /// ACL / lifecycle policy version on a component is older than the manifest.
    StaleAclPolicy {
        expected_version: String,
        observed_version: String,
    },
    /// A generation directory/object exists without a publication manifest.
    OrphanGeneration {
        generation: StorageGeneration,
        location_hint: String,
    },
    /// Incomplete generation still present past a lease/timeout (placeholder).
    IncompleteGeneration {
        generation: StorageGeneration,
        status: String,
    },
}

/// One typed reconciliation finding.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ReconciliationFinding {
    pub schema_version: u32,
    pub severity: ReconciliationSeverity,
    pub finding: ReconciliationFindingKind,
    /// Human-readable explanation (not a substitute for the typed kind).
    pub message: String,
}

impl ReconciliationFinding {
    pub fn new(
        severity: ReconciliationSeverity,
        finding: ReconciliationFindingKind,
        message: impl Into<String>,
    ) -> StorageResult<Self> {
        let message = message.into();
        if message.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "reconciliation finding message must not be empty",
            ));
        }
        Ok(Self {
            schema_version: INDEX_PUBLICATION_SCHEMA_VERSION,
            severity,
            finding,
            message,
        })
    }

    pub fn divergent_component(
        component: ComponentKind,
        expected_generation: StorageGeneration,
        observed_generation: StorageGeneration,
    ) -> StorageResult<Self> {
        Self::new(
            ReconciliationSeverity::Critical,
            ReconciliationFindingKind::DivergentComponent {
                component,
                expected_generation,
                observed_generation,
            },
            format!(
                "component {} generation {} diverges from expected {}",
                component.as_str(),
                observed_generation,
                expected_generation
            ),
        )
    }

    pub fn missing_chunk(
        chunk_id: impl Into<String>,
        generation: StorageGeneration,
    ) -> StorageResult<Self> {
        let chunk_id = chunk_id.into();
        if chunk_id.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "missing_chunk chunk_id must not be empty",
            ));
        }
        Self::new(
            ReconciliationSeverity::Error,
            ReconciliationFindingKind::MissingChunk {
                chunk_id: chunk_id.clone(),
                generation,
            },
            format!("chunk {chunk_id} missing under generation {generation}"),
        )
    }

    pub fn hash_mismatch(
        component: ComponentKind,
        expected_digest: impl Into<String>,
        observed_digest: impl Into<String>,
    ) -> StorageResult<Self> {
        let expected_digest = expected_digest.into();
        let observed_digest = observed_digest.into();
        Self::new(
            ReconciliationSeverity::Critical,
            ReconciliationFindingKind::HashMismatch {
                component,
                expected_digest: expected_digest.clone(),
                observed_digest: observed_digest.clone(),
            },
            format!(
                "component {} hash mismatch (expected {expected_digest}, observed {observed_digest})",
                component.as_str()
            ),
        )
    }

    pub fn stale_acl_policy(
        expected_version: impl Into<String>,
        observed_version: impl Into<String>,
    ) -> StorageResult<Self> {
        let expected_version = expected_version.into();
        let observed_version = observed_version.into();
        Self::new(
            ReconciliationSeverity::Error,
            ReconciliationFindingKind::StaleAclPolicy {
                expected_version: expected_version.clone(),
                observed_version: observed_version.clone(),
            },
            format!(
                "ACL policy version {observed_version} is stale vs expected {expected_version}"
            ),
        )
    }

    pub fn orphan_generation(
        generation: StorageGeneration,
        location_hint: impl Into<String>,
    ) -> StorageResult<Self> {
        let location_hint = location_hint.into();
        Self::new(
            ReconciliationSeverity::Warning,
            ReconciliationFindingKind::OrphanGeneration {
                generation,
                location_hint: location_hint.clone(),
            },
            format!("orphan generation {generation} at {location_hint}"),
        )
    }
}

/// Bundle of findings from one reconciliation pass.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct ReconciliationReport {
    pub schema_version: u32,
    pub findings: Vec<ReconciliationFinding>,
}

impl ReconciliationReport {
    pub fn new() -> Self {
        Self {
            schema_version: INDEX_PUBLICATION_SCHEMA_VERSION,
            findings: Vec::new(),
        }
    }

    pub fn push(&mut self, finding: ReconciliationFinding) {
        self.findings.push(finding);
    }

    pub fn is_clean(&self) -> bool {
        self.findings.is_empty()
    }

    pub fn has_critical(&self) -> bool {
        self.findings
            .iter()
            .any(|f| matches!(f.severity, ReconciliationSeverity::Critical))
    }
}
