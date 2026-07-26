//! Versioned index publication manifest and component digests.

use serde::{Deserialize, Serialize};

use crate::storage_ports::{StorageError, StorageGeneration, StorageResult};
use crate::types::EmbeddingProfileId;

/// Wire schema version for index publication documents in this module.
///
/// Unknown versions fail closed on decode rather than being accepted as
/// current-schema manifests.
pub const INDEX_PUBLICATION_SCHEMA_VERSION: u32 = 1;

/// Lifecycle status of a staged or published generation.
///
/// Only [`BuildStatus::Ready`] generations may be promoted to active.
/// Incomplete / failed / rolled-back / quarantined generations cannot become
/// active.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BuildStatus {
    /// Artifacts are still being written into the staging generation.
    Building,
    /// Artifacts written; integrity checks in progress.
    Validating,
    /// Fully validated and eligible for promotion (not yet active).
    Ready,
    /// Currently the active queryable generation.
    Active,
    /// Previously active; retained for rollback according to policy.
    RolledBack,
    /// Isolated as unsafe / orphaned / incomplete after reconciliation.
    Quarantined,
    /// Terminal failure; must not promote.
    Failed,
}

impl BuildStatus {
    /// Whether this status is eligible to become the active generation.
    pub fn can_promote(self) -> bool {
        matches!(self, Self::Ready)
    }

    /// Whether this status represents an incomplete (non-terminal success) build.
    pub fn is_incomplete(self) -> bool {
        matches!(self, Self::Building | Self::Validating)
    }

    /// Machine-readable status name.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Building => "building",
            Self::Validating => "validating",
            Self::Ready => "ready",
            Self::Active => "active",
            Self::RolledBack => "rolled_back",
            Self::Quarantined => "quarantined",
            Self::Failed => "failed",
        }
    }
}

/// Derived index capability kind referenced by a publication.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ComponentKind {
    Evidence,
    Catalog,
    Lexical,
    Vector,
    Graph,
}

impl ComponentKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Evidence => "evidence",
            Self::Catalog => "catalog",
            Self::Lexical => "lexical",
            Self::Vector => "vector",
            Self::Graph => "graph",
        }
    }
}

/// Opaque content digest for a published component (hex SHA-256 preferred).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ComponentDigest {
    pub kind: ComponentKind,
    /// Generation id of the component artifact (must match the publication
    /// generation when referential integrity is enforced for that component).
    pub generation: StorageGeneration,
    /// Non-empty content hash / digest string.
    pub digest: String,
}

impl ComponentDigest {
    pub fn new(
        kind: ComponentKind,
        generation: StorageGeneration,
        digest: impl Into<String>,
    ) -> StorageResult<Self> {
        let digest = digest.into();
        validate_non_empty_digest(&digest)?;
        Ok(Self {
            kind,
            generation,
            digest,
        })
    }

    pub fn validate(&self) -> StorageResult<()> {
        validate_non_empty_digest(&self.digest)
    }
}

/// Opaque source snapshot reference bound into a publication.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SourceSnapshotRef {
    /// Opaque snapshot / source-set id (adapter-defined).
    pub snapshot_id: String,
    /// Content digest of the snapshot set (non-empty).
    pub digest: String,
}

impl SourceSnapshotRef {
    pub fn new(snapshot_id: impl Into<String>, digest: impl Into<String>) -> StorageResult<Self> {
        let snapshot_id = snapshot_id.into();
        let digest = digest.into();
        if snapshot_id.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "source snapshot_id must not be empty",
            ));
        }
        validate_non_empty_digest(&digest)?;
        Ok(Self {
            snapshot_id,
            digest,
        })
    }

    pub fn validate(&self) -> StorageResult<()> {
        if self.snapshot_id.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "source snapshot_id must not be empty",
            ));
        }
        validate_non_empty_digest(&self.digest)
    }
}

/// Declared capabilities that a publication generation is expected to serve.
///
/// Completeness validation requires matching component digests for each
/// declared capability.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
pub struct DeclaredCapabilities {
    pub evidence: bool,
    pub catalog: bool,
    pub lexical: bool,
    pub vector: bool,
    pub graph: bool,
}

impl DeclaredCapabilities {
    pub fn all_core() -> Self {
        Self {
            evidence: true,
            catalog: true,
            lexical: true,
            vector: true,
            graph: false,
        }
    }

    pub fn required_kinds(&self) -> Vec<ComponentKind> {
        let mut kinds = Vec::new();
        if self.evidence {
            kinds.push(ComponentKind::Evidence);
        }
        if self.catalog {
            kinds.push(ComponentKind::Catalog);
        }
        if self.lexical {
            kinds.push(ComponentKind::Lexical);
        }
        if self.vector {
            kinds.push(ComponentKind::Vector);
        }
        if self.graph {
            kinds.push(ComponentKind::Graph);
        }
        kinds
    }
}

/// Full index publication manifest for one staged/published generation.
///
/// This is the DIST-006 document: multi-component, hash-bound, status-aware.
/// The thinner [`crate::storage_ports::PublicationManifest`] remains the
/// port-level publish record and may be projected from this document later.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct IndexPublicationManifest {
    pub schema_version: u32,
    /// Publication / generation id (fences queries and CAS promotion).
    pub generation: StorageGeneration,
    /// Source snapshot set bound into this generation.
    pub source_snapshots: Vec<SourceSnapshotRef>,
    /// Evidence store generation included in this publication.
    pub evidence_generation: StorageGeneration,
    /// Catalog generation included in this publication.
    pub catalog_generation: StorageGeneration,
    /// Optional lexical index generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lexical_generation: Option<StorageGeneration>,
    /// Optional vector index generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub vector_generation: Option<StorageGeneration>,
    /// Optional graph artifact generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub graph_generation: Option<StorageGeneration>,
    /// Embedding / retrieval profile for vector (and related) components.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub profile_id: Option<EmbeddingProfileId>,
    /// Optional embedding model identifier (adapter-defined).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub embedding_model_id: Option<String>,
    /// Per-component content hashes.
    pub component_digests: Vec<ComponentDigest>,
    /// Capabilities this generation claims to serve.
    pub capabilities: DeclaredCapabilities,
    /// ACL / lifecycle policy version fencing authorization metadata.
    pub acl_policy_version: String,
    /// Lifecycle / retention policy version (may equal ACL policy).
    pub lifecycle_policy_version: String,
    /// Build / publication status.
    pub status: BuildStatus,
    /// Wall-clock build / publish timestamp (RFC3339 or adapter-defined).
    pub built_at: String,
    /// Optional free-form build metadata (builder id, host, etc.).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub build_metadata: Option<String>,
}

/// Constructor field bag keeping [`IndexPublicationManifest::new`] arity small.
#[derive(Debug, Clone)]
pub struct IndexPublicationManifestFields {
    pub generation: StorageGeneration,
    pub source_snapshots: Vec<SourceSnapshotRef>,
    pub evidence_generation: StorageGeneration,
    pub catalog_generation: StorageGeneration,
    pub lexical_generation: Option<StorageGeneration>,
    pub vector_generation: Option<StorageGeneration>,
    pub graph_generation: Option<StorageGeneration>,
    pub profile_id: Option<EmbeddingProfileId>,
    pub embedding_model_id: Option<String>,
    pub component_digests: Vec<ComponentDigest>,
    pub capabilities: DeclaredCapabilities,
    pub acl_policy_version: String,
    pub lifecycle_policy_version: String,
    pub status: BuildStatus,
    pub built_at: String,
    pub build_metadata: Option<String>,
}

impl IndexPublicationManifest {
    /// Build a current-schema manifest and validate structural invariants.
    pub fn new(fields: IndexPublicationManifestFields) -> StorageResult<Self> {
        let manifest = Self {
            schema_version: INDEX_PUBLICATION_SCHEMA_VERSION,
            generation: fields.generation,
            source_snapshots: fields.source_snapshots,
            evidence_generation: fields.evidence_generation,
            catalog_generation: fields.catalog_generation,
            lexical_generation: fields.lexical_generation,
            vector_generation: fields.vector_generation,
            graph_generation: fields.graph_generation,
            profile_id: fields.profile_id,
            embedding_model_id: fields.embedding_model_id,
            component_digests: fields.component_digests,
            capabilities: fields.capabilities,
            acl_policy_version: fields.acl_policy_version,
            lifecycle_policy_version: fields.lifecycle_policy_version,
            status: fields.status,
            built_at: fields.built_at,
            build_metadata: fields.build_metadata,
        };
        manifest.validate_structure()?;
        Ok(manifest)
    }

    /// Structural validation (schema, non-empty policy strings, digests).
    pub fn validate_structure(&self) -> StorageResult<()> {
        validate_schema_version(self.schema_version)?;
        if self.acl_policy_version.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "acl_policy_version must not be empty",
            ));
        }
        if self.lifecycle_policy_version.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "lifecycle_policy_version must not be empty",
            ));
        }
        if self.built_at.trim().is_empty() {
            return Err(StorageError::invalid_request("built_at must not be empty"));
        }
        if self.source_snapshots.is_empty() {
            return Err(StorageError::invalid_request(
                "source_snapshots must not be empty",
            ));
        }
        for snap in &self.source_snapshots {
            snap.validate()?;
        }
        for digest in &self.component_digests {
            digest.validate()?;
        }
        if self.capabilities.vector && self.profile_id.is_none() {
            return Err(StorageError::invalid_request(
                "vector capability requires profile_id",
            ));
        }
        Ok(())
    }

    pub fn with_status(mut self, status: BuildStatus) -> Self {
        self.status = status;
        self
    }

    pub fn component(&self, kind: ComponentKind) -> Option<&ComponentDigest> {
        self.component_digests.iter().find(|c| c.kind == kind)
    }
}

/// Decode a JSON index publication manifest, failing closed on unknown schema.
pub fn decode_index_publication_manifest_json(
    bytes: &[u8],
) -> StorageResult<IndexPublicationManifest> {
    let value: IndexPublicationManifest = serde_json::from_slice(bytes).map_err(|err| {
        StorageError::invalid_request(format!("index publication manifest decode: {err}"))
    })?;
    value.validate_structure()?;
    Ok(value)
}

fn validate_schema_version(version: u32) -> StorageResult<()> {
    if version == 0 {
        return Err(StorageError::invalid_request(
            "index publication schema_version must be > 0",
        ));
    }
    if version != INDEX_PUBLICATION_SCHEMA_VERSION {
        return Err(StorageError::invalid_request(format!(
            "unsupported index publication schema_version {version}; expected {INDEX_PUBLICATION_SCHEMA_VERSION}"
        )));
    }
    Ok(())
}

fn validate_non_empty_digest(digest: &str) -> StorageResult<()> {
    if digest.trim().is_empty() {
        return Err(StorageError::invalid_request(
            "component/source digest must not be empty",
        ));
    }
    // Prefer hex-like digests (≥16 hex chars) but allow opaque adapter digests
    // that are non-empty and free of whitespace.
    if digest.chars().any(|c| c.is_whitespace()) {
        return Err(StorageError::invalid_request(
            "component/source digest must not contain whitespace",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod unit_tests {
    use super::*;

    #[test]
    fn build_status_promote_gate() {
        assert!(BuildStatus::Ready.can_promote());
        assert!(!BuildStatus::Building.can_promote());
        assert!(!BuildStatus::Failed.can_promote());
        assert!(BuildStatus::Building.is_incomplete());
    }
}
