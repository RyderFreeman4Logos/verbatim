//! Idempotent batch-upsert and tombstone operation boundary types.

use std::collections::BTreeSet;
use std::fmt;

use crate::diskann3::PublicationGeneration;

use super::{
    ChunkIdMapping, DiskAnnBackendDiagnosticCode, DiskAnnBackendError, DiskAnnBackendResult,
    GenerationContext, StableVectorId, VectorInput,
};

/// Opaque bounded idempotency key. Its debug form intentionally never exposes the key value.
#[derive(Clone, PartialEq, Eq, Hash)]
pub struct IdempotencyKey(String);

impl IdempotencyKey {
    /// Creates a bounded, printable opaque key suitable for a provider's idempotency ledger.
    pub fn new(value: impl Into<String>) -> DiskAnnBackendResult<Self> {
        let value = value.into();
        if value.is_empty()
            || value.len() > 128
            || !value.is_ascii()
            || value.bytes().any(|byte| byte.is_ascii_control())
        {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidIdempotencyKey,
            ));
        }
        Ok(Self(value))
    }

    /// Returns the opaque key only to the adapter implementation that owns the ledger.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Debug for IdempotencyKey {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str("IdempotencyKey(REDACTED)")
    }
}

/// One stable-ID full-precision vector write.
#[derive(Clone, PartialEq)]
pub struct VectorUpsert {
    vector_id: StableVectorId,
    input: VectorInput,
}

impl VectorUpsert {
    /// Binds an incoming full-precision vector to its immutable stable identity.
    pub const fn new(vector_id: StableVectorId, input: VectorInput) -> Self {
        Self { vector_id, input }
    }

    /// Returns the stable vector identity.
    pub const fn vector_id(&self) -> StableVectorId {
        self.vector_id
    }

    /// Returns the profile- and generation-bound full-precision vector.
    pub const fn input(&self) -> &VectorInput {
        &self.input
    }
}

/// Bounded idempotent vector upsert batch bound to one chunk-ID mapping version.
#[derive(Clone, PartialEq)]
pub struct BatchUpsertRequest {
    context: GenerationContext,
    mapping: ChunkIdMapping,
    upserts: Vec<VectorUpsert>,
    idempotency_key: IdempotencyKey,
}

impl BatchUpsertRequest {
    /// Prevents duplicate IDs, mismatched mappings, and malformed vectors from reaching a provider.
    pub fn new(
        context: GenerationContext,
        mapping: ChunkIdMapping,
        upserts: Vec<VectorUpsert>,
        idempotency_key: IdempotencyKey,
    ) -> DiskAnnBackendResult<Self> {
        if upserts.is_empty() || upserts.len() > Self::MAX_UPSERTS {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidMutationBatch,
            ));
        }
        mapping.validate_binding(context.vector_space(), context.generation())?;
        let mut vector_ids = BTreeSet::new();
        for upsert in &upserts {
            if !vector_ids.insert(upsert.vector_id) {
                return Err(DiskAnnBackendError::contract(
                    DiskAnnBackendDiagnosticCode::DuplicateMutationVectorId,
                ));
            }
            if mapping.chunk_id(upsert.vector_id).is_none() {
                return Err(DiskAnnBackendError::contract(
                    DiskAnnBackendDiagnosticCode::InvalidChunkIdMapping,
                ));
            }
            context.validate_input(&upsert.input)?;
        }
        Ok(Self {
            context,
            mapping,
            upserts,
            idempotency_key,
        })
    }

    /// Upper bound on one durable mutation operation.
    pub const MAX_UPSERTS: usize = 10_000;

    /// Returns the immutable generation context.
    pub const fn context(&self) -> &GenerationContext {
        &self.context
    }

    /// Returns the versioned stable-ID-to-chunk-ID mapping.
    pub const fn mapping(&self) -> &ChunkIdMapping {
        &self.mapping
    }

    /// Returns the bounded write batch.
    pub fn upserts(&self) -> &[VectorUpsert] {
        &self.upserts
    }

    /// Returns the opaque idempotency key for adapter-local ledger lookup.
    pub const fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }
}

/// Bounded idempotent tombstone batch by stable vector identity.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TombstoneBatchRequest {
    context: GenerationContext,
    vector_ids: Vec<StableVectorId>,
    idempotency_key: IdempotencyKey,
}

impl TombstoneBatchRequest {
    /// Rejects empty or duplicate tombstone batches before persistence.
    pub fn new(
        context: GenerationContext,
        vector_ids: Vec<StableVectorId>,
        idempotency_key: IdempotencyKey,
    ) -> DiskAnnBackendResult<Self> {
        if vector_ids.is_empty() || vector_ids.len() > Self::MAX_TOMBSTONES {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::InvalidMutationBatch,
            ));
        }
        let mut seen = BTreeSet::new();
        if vector_ids.iter().any(|vector_id| !seen.insert(*vector_id)) {
            return Err(DiskAnnBackendError::contract(
                DiskAnnBackendDiagnosticCode::DuplicateMutationVectorId,
            ));
        }
        Ok(Self {
            context,
            vector_ids,
            idempotency_key,
        })
    }

    /// Upper bound on one durable deletion operation.
    pub const MAX_TOMBSTONES: usize = 10_000;

    /// Returns the immutable generation context.
    pub const fn context(&self) -> &GenerationContext {
        &self.context
    }

    /// Returns the stable IDs to tombstone exactly once.
    pub fn vector_ids(&self) -> &[StableVectorId] {
        &self.vector_ids
    }

    /// Returns the opaque idempotency key for adapter-local ledger lookup.
    pub const fn idempotency_key(&self) -> &IdempotencyKey {
        &self.idempotency_key
    }
}

/// Outcome of an idempotent mutation operation.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MutationReceipt {
    generation: PublicationGeneration,
    replayed: bool,
}

impl MutationReceipt {
    /// Creates a bounded mutation receipt without including user-provided IDs or vectors.
    pub const fn new(generation: PublicationGeneration, replayed: bool) -> Self {
        Self {
            generation,
            replayed,
        }
    }

    /// Returns the generation that received the mutation.
    pub const fn generation(&self) -> PublicationGeneration {
        self.generation
    }

    /// Returns whether an existing idempotent mutation result was replayed.
    pub const fn replayed(&self) -> bool {
        self.replayed
    }
}
