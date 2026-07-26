//! Durable active-generation pointer with CAS epoch semantics.

use serde::{Deserialize, Serialize};

use crate::storage_ports::{StorageError, StorageGeneration, StorageResult};

use super::manifest::INDEX_PUBLICATION_SCHEMA_VERSION;

fn validate_schema_version(version: u32) -> StorageResult<()> {
    if version == 0 {
        return Err(StorageError::invalid_request(
            "active generation pointer schema_version must be > 0",
        ));
    }
    if version != INDEX_PUBLICATION_SCHEMA_VERSION {
        return Err(StorageError::invalid_request(format!(
            "unsupported active generation pointer schema_version {version}; expected {INDEX_PUBLICATION_SCHEMA_VERSION}"
        )));
    }
    Ok(())
}

/// Monotonic pointer epoch used for compare-and-swap promotion.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
pub struct PointerEpoch(pub u64);

impl PointerEpoch {
    pub const INITIAL: Self = Self(0);

    pub const fn new(value: u64) -> Self {
        Self(value)
    }

    pub fn next(self) -> Self {
        Self(self.0.saturating_add(1))
    }
}

impl std::fmt::Display for PointerEpoch {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.0)
    }
}

/// Durable pointer to the single active publication generation.
///
/// Promotion is CAS on `(active_generation, epoch)`: a concurrent promoter
/// observing a stale epoch loses with a typed conflict.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ActiveGenerationPointer {
    pub schema_version: u32,
    /// Currently active publication generation.
    pub active_generation: StorageGeneration,
    /// CAS epoch; increments on every successful promote or rollback.
    pub epoch: PointerEpoch,
    /// Optional previous generation retained for rollback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub previous_generation: Option<StorageGeneration>,
    /// Wall-clock of last pointer mutation (adapter-defined).
    pub updated_at: String,
}

impl ActiveGenerationPointer {
    pub fn new(
        active_generation: StorageGeneration,
        epoch: PointerEpoch,
        updated_at: impl Into<String>,
    ) -> StorageResult<Self> {
        let updated_at = updated_at.into();
        if updated_at.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "active generation pointer updated_at must not be empty",
            ));
        }
        Ok(Self {
            schema_version: INDEX_PUBLICATION_SCHEMA_VERSION,
            active_generation,
            epoch,
            previous_generation: None,
            updated_at,
        })
    }

    pub fn with_previous(mut self, previous: StorageGeneration) -> Self {
        self.previous_generation = Some(previous);
        self
    }

    pub fn validate(&self) -> StorageResult<()> {
        validate_schema_version(self.schema_version)?;
        if self.updated_at.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "active generation pointer updated_at must not be empty",
            ));
        }
        Ok(())
    }
}

/// Decode a JSON active-generation pointer, failing closed on unknown schema.
pub fn decode_active_generation_pointer_json(
    bytes: &[u8],
) -> StorageResult<ActiveGenerationPointer> {
    let value: ActiveGenerationPointer = serde_json::from_slice(bytes).map_err(|err| {
        StorageError::invalid_request(format!("active generation pointer decode: {err}"))
    })?;
    value.validate()?;
    Ok(value)
}
