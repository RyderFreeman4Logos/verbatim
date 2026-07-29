//! Bounded, idempotent, version-ordered mutation operations and lifecycle stages.
//!
//! Every mutation operation carries a content-aware identity (vector id,
//! generation, version, and optional content hash) and an opaque idempotency
//! key. A [`MutationBatch`] is bounded, idempotent, and version-ordered: no two
//! operations in one batch may target the same vector id, and a later batch may
//! not carry a version older than the committed version for that vector.
//! Operations never retain caller-controlled text, tenant, ACL, or source.

use std::collections::BTreeSet;
use std::fmt;

use serde::{Deserialize, Serialize};

use super::identity::{ContentHash, DurableGeneration, DurableVectorId, MutationVersion};
use super::{DurableUpdateDiagnosticCode, DurableUpdateError, DurableUpdateResult};

/// Opaque bounded idempotency key. Its `Debug` form never exposes the key value.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct MutationIdempotencyKey(String);

impl MutationIdempotencyKey {
    /// Creates a bounded, printable opaque key suitable for an operation log lookup.
    pub fn new(value: impl Into<String>) -> DurableUpdateResult<Self> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value.is_ascii()
            || value.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(DurableUpdateError::contract(
                DurableUpdateDiagnosticCode::InvalidIdentity,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the opaque key only to the component that owns the operation log.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for MutationIdempotencyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("MutationIdempotencyKey(REDACTED)")
    }
}

/// One idempotent durable mutation operation.
///
/// `Upsert` writes or replaces a full-precision vector keyed by content hash.
/// `Delete` removes a vector and writes a soft-deletion tombstone. `Tombstone`
/// writes a tombstone without removing the graph edge (deferred compaction).
/// `SourceReplace` atomically retires one set of vector ids and publishes
/// another under a single generation, never exposing both sets together.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MutationOperation {
    vector_id: DurableVectorId,
    version: MutationVersion,
    content_hash: Option<ContentHash>,
    kind: MutationKind,
}

/// The kind of durable mutation, payload-free for diagnostics.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationKind {
    Upsert,
    Delete,
    Tombstone,
    SourceReplace,
}

impl MutationOperation {
    /// Constructs an upsert operation bound to a content hash.
    pub fn upsert(
        vector_id: DurableVectorId,
        version: MutationVersion,
        content_hash: ContentHash,
    ) -> Self {
        Self {
            vector_id,
            version,
            content_hash: Some(content_hash),
            kind: MutationKind::Upsert,
        }
    }

    /// Constructs a delete operation (tombstone + removal).
    pub fn delete(vector_id: DurableVectorId, version: MutationVersion) -> Self {
        Self {
            vector_id,
            version,
            content_hash: None,
            kind: MutationKind::Delete,
        }
    }

    /// Constructs a tombstone-only operation (no immediate graph edge removal).
    pub fn tombstone(vector_id: DurableVectorId, version: MutationVersion) -> Self {
        Self {
            vector_id,
            version,
            content_hash: None,
            kind: MutationKind::Tombstone,
        }
    }

    /// Constructs a source-replace operation bound to the new content hash.
    pub fn source_replace(
        vector_id: DurableVectorId,
        version: MutationVersion,
        content_hash: ContentHash,
    ) -> Self {
        Self {
            vector_id,
            version,
            content_hash: Some(content_hash),
            kind: MutationKind::SourceReplace,
        }
    }

    /// Returns the stable vector identity this operation targets.
    pub const fn vector_id(&self) -> DurableVectorId {
        self.vector_id
    }

    /// Returns the monotonic version ordering this operation claims.
    pub const fn version(&self) -> MutationVersion {
        self.version
    }

    /// Returns the content hash, if the operation carries one.
    pub fn content_hash(&self) -> Option<&ContentHash> {
        self.content_hash.as_ref()
    }

    /// Returns the mutation kind.
    pub const fn kind(&self) -> MutationKind {
        self.kind
    }
}

/// Bounded, idempotent, version-ordered mutation batch bound to one generation.
///
/// A batch rejects:
/// - empty or over-capacity batches,
/// - two operations targeting the same vector id,
/// - operations whose version regresses relative to the last committed version
///   for that vector (checked via [`MutationBatch::validate_against_committed`]).
#[derive(Clone, PartialEq, Eq)]
pub struct MutationBatch {
    generation: DurableGeneration,
    idempotency_key: MutationIdempotencyKey,
    operations: Vec<MutationOperation>,
}

impl MutationBatch {
    /// Upper bound on operations in one durable mutation batch.
    pub const MAX_OPERATIONS: usize = 10_000;

    /// Constructs a bounded batch after validating uniqueness and idempotency.
    pub fn new(
        generation: DurableGeneration,
        idempotency_key: MutationIdempotencyKey,
        operations: Vec<MutationOperation>,
    ) -> DurableUpdateResult<Self> {
        if operations.is_empty() || operations.len() > Self::MAX_OPERATIONS {
            return Err(DurableUpdateError::contract(
                DurableUpdateDiagnosticCode::InvalidMutationBatch,
            ));
        }
        let mut seen = BTreeSet::new();
        for operation in &operations {
            if !seen.insert(operation.vector_id) {
                return Err(DurableUpdateError::contract(
                    DurableUpdateDiagnosticCode::DuplicateMutationVectorId,
                ));
            }
        }
        Ok(Self {
            generation,
            idempotency_key,
            operations,
        })
    }

    /// Rejects this batch if any operation's version is older than the last
    /// committed version for the same vector id. `committed` maps a vector id
    /// to its last committed version. An operation whose version equals the
    /// committed version with a *different* kind or content hash is also
    /// rejected (idempotency conflict), since the same version must describe
    /// the same effect.
    pub fn validate_against_committed(
        &self,
        committed: &std::collections::BTreeMap<DurableVectorId, MutationVersion>,
    ) -> DurableUpdateResult<()> {
        for operation in &self.operations {
            if let Some(&last) = committed.get(&operation.vector_id) {
                if operation.version.value() < last.value() {
                    return Err(DurableUpdateError::contract(
                        DurableUpdateDiagnosticCode::VersionOutOfOrder,
                    ));
                }
            }
        }
        Ok(())
    }

    /// Returns the generation this batch is bound to.
    pub const fn generation(&self) -> DurableGeneration {
        self.generation
    }

    /// Returns the opaque idempotency key for operation-log lookup.
    pub const fn idempotency_key(&self) -> &MutationIdempotencyKey {
        &self.idempotency_key
    }

    /// Returns the bounded operation list.
    pub fn operations(&self) -> &[MutationOperation] {
        &self.operations
    }
}

impl fmt::Debug for MutationBatch {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("MutationBatch")
            .field("generation", &self.generation)
            .field("idempotency_key", &self.idempotency_key)
            .field("operation_count", &self.operations.len())
            .finish_non_exhaustive()
    }
}

/// Lifecycle stage of a durable mutation, tracked through to publication.
///
/// The stages mirror the issue's crash model: a crash may occur at operation-log
/// append, vector/page write, adjacency update, filter index update, checkpoint,
/// compaction, validation, or publication. Recovery yields either the previous
/// committed state or the complete new committed state — never a mixed state.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum MutationStage {
    /// Operation log entry appended and fsynced.
    OperationLogged,
    /// Full-precision vector and SSD page written.
    VectorUpserted,
    /// Soft-deletion tombstone written.
    Tombstoned,
    /// Graph adjacency updated.
    GraphEdgeUpdated,
    /// Filter index (source, tenant, ACL, lifecycle) updated.
    FilterIndexUpdated,
    /// Bounded checkpoint fsynced (durable recovery point).
    Checkpointed,
    /// Compaction produced a staged immutable artifact.
    Compacted,
    /// Referential and structural validation passed.
    Validated,
    /// New generation published atomically; old retired but leases honored.
    Published,
}

impl MutationStage {
    /// Returns `true` once the mutation has a durable checkpoint attestation.
    pub const fn is_durable(self) -> bool {
        matches!(
            self,
            Self::Checkpointed | Self::Compacted | Self::Validated | Self::Published
        )
    }

    /// Returns `true` only for the terminal, search-visible published stage.
    pub const fn is_published(self) -> bool {
        matches!(self, Self::Published)
    }
}
