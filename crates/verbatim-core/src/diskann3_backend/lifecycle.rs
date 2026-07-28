//! Staged-generation lifecycle, snapshot/recovery, and deterministic-shutdown contract types.

use std::fmt;

use crate::diskann3::ShardId;

use super::{
    DiskAnnBackendDiagnosticCode, DiskAnnBackendError, DiskAnnBackendResult, GenerationContext,
};

/// One validated shard-generation request for stage/build/load/validate operations.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShardGenerationRequest {
    context: GenerationContext,
    shard: ShardId,
}

impl ShardGenerationRequest {
    /// Accepts only a prevalidated DiskANN3 shard identity and a generation-bound context.
    pub fn new(context: GenerationContext, shard: ShardId) -> DiskAnnBackendResult<Self> {
        shard.validate().map_err(|_| {
            DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidShardGenerationRequest,
            )
        })?;
        if &shard.vector_space != context.vector_space().vector_space_id() {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::VectorSpaceMismatch,
            ));
        }
        if shard.generation != context.generation() {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::GenerationMismatch,
            ));
        }
        Ok(Self { context, shard })
    }

    /// Returns the operation's immutable generation context.
    pub const fn context(&self) -> &GenerationContext {
        &self.context
    }

    /// Returns the opaque, validated shard identity.
    pub const fn shard(&self) -> &ShardId {
        &self.shard
    }
}

/// A receipt for a successful shard lifecycle step.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationReceipt {
    context: GenerationContext,
    shard: ShardId,
}

impl GenerationReceipt {
    /// Creates a receipt that preserves generation and shard identity without provider internals.
    pub fn new(context: GenerationContext, shard: ShardId) -> DiskAnnBackendResult<Self> {
        ShardGenerationRequest::new(context.clone(), shard.clone())?;
        Ok(Self { context, shard })
    }

    /// Returns the immutable generation context.
    pub const fn context(&self) -> &GenerationContext {
        &self.context
    }

    /// Returns the staged/build/loaded shard identity.
    pub const fn shard(&self) -> &ShardId {
        &self.shard
    }
}

/// Observable lifecycle state for a published generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GenerationLifecycleState {
    Staged,
    Building,
    Loaded,
    Validated,
    Published,
    Retired,
}

/// Bounded status information for one generation, never provider paths or internal handles.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct GenerationStatus {
    context: GenerationContext,
    state: GenerationLifecycleState,
    indexed_vector_count: u64,
}

impl GenerationStatus {
    /// Creates a status response for the caller's selected generation.
    pub const fn new(
        context: GenerationContext,
        state: GenerationLifecycleState,
        indexed_vector_count: u64,
    ) -> Self {
        Self {
            context,
            state,
            indexed_vector_count,
        }
    }

    /// Returns the immutable generation context.
    pub const fn context(&self) -> &GenerationContext {
        &self.context
    }

    /// Returns the lifecycle state.
    pub const fn state(&self) -> GenerationLifecycleState {
        self.state
    }

    /// Returns the bounded aggregate vector count.
    pub const fn indexed_vector_count(&self) -> u64 {
        self.indexed_vector_count
    }
}

/// Bounded identifier for a provider-independent restore point.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct SnapshotId(String);

impl SnapshotId {
    /// Creates an opaque ASCII snapshot identifier without encoding a filesystem path.
    pub fn new(value: impl Into<String>) -> DiskAnnBackendResult<Self> {
        let value = value.into();
        let allowed = value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_'));
        if value.is_empty() || value.len() > 128 || !allowed {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidSnapshotId,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the opaque adapter snapshot token.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for SnapshotId {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("SnapshotId(REDACTED)")
    }
}

/// Snapshot receipt bound to exactly one published generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SnapshotReceipt {
    context: GenerationContext,
    snapshot_id: SnapshotId,
}

impl SnapshotReceipt {
    /// Creates a snapshot receipt without exposing provider storage locations.
    pub const fn new(context: GenerationContext, snapshot_id: SnapshotId) -> Self {
        Self {
            context,
            snapshot_id,
        }
    }

    /// Returns the immutable generation context.
    pub const fn context(&self) -> &GenerationContext {
        &self.context
    }

    /// Returns the opaque snapshot identity.
    pub const fn snapshot_id(&self) -> &SnapshotId {
        &self.snapshot_id
    }
}

/// Recovery source selected by the caller under a bounded generation context.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoverySource {
    Snapshot(SnapshotId),
    RebuildFromAuthoritativeVectors,
}

/// Explicit request to restore a snapshot or reproducibly rebuild from authoritative vectors.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreOrRebuildRequest {
    context: GenerationContext,
    source: RecoverySource,
}

impl RestoreOrRebuildRequest {
    /// Keeps recovery scoped to a specific validated generation.
    pub const fn new(context: GenerationContext, source: RecoverySource) -> Self {
        Self { context, source }
    }

    /// Returns the immutable generation context.
    pub const fn context(&self) -> &GenerationContext {
        &self.context
    }

    /// Returns the selected bounded recovery source.
    pub const fn source(&self) -> &RecoverySource {
        &self.source
    }
}

/// Receipt proving a deterministic adapter shutdown released its managed resources.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ShutdownReceipt;

impl ShutdownReceipt {
    /// Rejects a provider response that cannot attest all managed resources were released.
    pub fn released(resources_released: bool) -> DiskAnnBackendResult<Self> {
        if !resources_released {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::ShutdownNotComplete,
            ));
        }
        Ok(Self)
    }
}
