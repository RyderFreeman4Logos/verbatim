//! Bind a query/run/cursor to exactly one publication generation.

use serde::{Deserialize, Serialize};

use crate::storage_ports::{StorageError, StorageGeneration, StorageResult};

use super::manifest::INDEX_PUBLICATION_SCHEMA_VERSION;
use super::pointer::PointerEpoch;

/// What kind of consumer is bound to a publication generation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum QueryPublicationBindingKind {
    /// Ad-hoc query request.
    Query,
    /// Durable run / job manifest.
    Run,
    /// Pagination cursor continuing a prior query/run.
    Cursor,
}

/// Fence that binds a query, run, or cursor to exactly one publication generation.
///
/// Later cursor/run manifests should embed this type so mixed-generation results
/// cannot be produced by rebinding mid-stream.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QueryPublicationBinding {
    pub schema_version: u32,
    pub kind: QueryPublicationBindingKind,
    /// Publication generation the consumer must read against.
    pub publication_generation: StorageGeneration,
    /// Optional pointer epoch observed when the binding was created.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pointer_epoch: Option<PointerEpoch>,
    /// Opaque consumer correlation id (request/run/cursor id).
    pub consumer_id: String,
}

impl QueryPublicationBinding {
    pub fn new(
        kind: QueryPublicationBindingKind,
        publication_generation: StorageGeneration,
        consumer_id: impl Into<String>,
    ) -> StorageResult<Self> {
        let consumer_id = consumer_id.into();
        if consumer_id.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "query publication binding consumer_id must not be empty",
            ));
        }
        Ok(Self {
            schema_version: INDEX_PUBLICATION_SCHEMA_VERSION,
            kind,
            publication_generation,
            pointer_epoch: None,
            consumer_id,
        })
    }

    pub fn with_pointer_epoch(mut self, epoch: PointerEpoch) -> Self {
        self.pointer_epoch = Some(epoch);
        self
    }

    pub fn validate(&self) -> StorageResult<()> {
        if self.schema_version == 0 {
            return Err(StorageError::invalid_request(
                "query publication binding schema_version must be > 0",
            ));
        }
        if self.schema_version != INDEX_PUBLICATION_SCHEMA_VERSION {
            return Err(StorageError::invalid_request(format!(
                "unsupported query publication binding schema_version {}; expected {INDEX_PUBLICATION_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        if self.consumer_id.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "query publication binding consumer_id must not be empty",
            ));
        }
        Ok(())
    }
}

/// Decode a JSON query publication binding, failing closed on unknown schema.
pub fn decode_query_publication_binding_json(
    bytes: &[u8],
) -> StorageResult<QueryPublicationBinding> {
    let value: QueryPublicationBinding = serde_json::from_slice(bytes).map_err(|err| {
        StorageError::invalid_request(format!("query publication binding decode: {err}"))
    })?;
    value.validate()?;
    Ok(value)
}
