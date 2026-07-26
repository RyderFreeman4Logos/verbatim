//! Composite remote request envelope (contract only).

use serde::{Deserialize, Serialize};

use crate::storage_ports::{StorageCapabilityKind, StorageError, StorageGeneration, StorageResult};

use super::bounds::RequestBounds;
use super::compat::{CompatibilityOffer, REMOTE_STORAGE_CLIENT_SCHEMA_VERSION};
use super::idempotency::RetryPolicy;
use super::identity::RemoteClientIdentity;
use super::stream::{BoundedPageRequest, StreamReadRequest};
use super::trace::RemoteTraceCarrier;

/// Named remote operation against a storage port capability.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteOperation {
    pub capability: StorageCapabilityKind,
    pub operation: String,
}

impl RemoteOperation {
    pub fn new(
        capability: StorageCapabilityKind,
        operation: impl Into<String>,
    ) -> StorageResult<Self> {
        let operation = operation.into();
        if operation.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "remote operation name must not be empty",
            ));
        }
        Ok(Self {
            capability,
            operation,
        })
    }

    pub fn validate(&self) -> StorageResult<()> {
        if self.operation.trim().is_empty() {
            return Err(StorageError::invalid_request(
                "remote operation name must not be empty",
            ));
        }
        Ok(())
    }
}

/// Pure request envelope carried by every remote storage/index call.
///
/// Transport adapters serialize this (plus operation-specific payload) and
/// enforce bounds; this type never performs IO.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteRequestEnvelope {
    pub schema_version: u32,
    pub identity: RemoteClientIdentity,
    pub operation: RemoteOperation,
    pub bounds: RequestBounds,
    pub retry: RetryPolicy,
    pub compatibility: CompatibilityOffer,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace: Option<RemoteTraceCarrier>,
    /// Expected publication/index generation fence when applicable.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected_generation: Option<StorageGeneration>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub page: Option<BoundedPageRequest>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub stream: Option<StreamReadRequest>,
}

impl RemoteRequestEnvelope {
    pub fn new(
        identity: RemoteClientIdentity,
        operation: RemoteOperation,
        bounds: RequestBounds,
        retry: RetryPolicy,
    ) -> StorageResult<Self> {
        let envelope = Self {
            schema_version: REMOTE_STORAGE_CLIENT_SCHEMA_VERSION,
            identity,
            operation,
            bounds,
            retry,
            compatibility: CompatibilityOffer::current(),
            trace: None,
            expected_generation: None,
            page: None,
            stream: None,
        };
        envelope.validate()?;
        Ok(envelope)
    }

    pub fn with_trace(mut self, trace: RemoteTraceCarrier) -> StorageResult<Self> {
        trace.validate()?;
        self.trace = Some(trace);
        Ok(self)
    }

    pub fn with_expected_generation(mut self, generation: StorageGeneration) -> Self {
        self.expected_generation = Some(generation);
        self
    }

    pub fn with_page(mut self, page: BoundedPageRequest) -> StorageResult<Self> {
        page.validate()?;
        self.page = Some(page);
        Ok(self)
    }

    pub fn with_stream(mut self, stream: StreamReadRequest) -> StorageResult<Self> {
        stream.validate()?;
        self.stream = Some(stream);
        Ok(self)
    }

    pub fn validate(&self) -> StorageResult<()> {
        if self.schema_version != REMOTE_STORAGE_CLIENT_SCHEMA_VERSION {
            return Err(StorageError::invalid_request(format!(
                "unsupported remote request envelope schema version {}; expected {REMOTE_STORAGE_CLIENT_SCHEMA_VERSION}",
                self.schema_version
            )));
        }
        self.identity.validate()?;
        self.operation.validate()?;
        self.bounds.validate()?;
        self.retry.validate()?;
        self.compatibility.validate()?;
        if let Some(trace) = &self.trace {
            trace.validate()?;
        }
        if let Some(page) = &self.page {
            page.validate()?;
        }
        if let Some(stream) = &self.stream {
            stream.validate()?;
        }
        Ok(())
    }

    /// Pre-flight authorization gate: unauthenticated identities cannot
    /// enumerate or fetch; readers cannot mutate.
    pub fn authorize_preflight(&self) -> StorageResult<()> {
        self.validate()?;
        if self.retry.kind.is_mutation() {
            self.identity.require_mutation(&self.operation.operation)?;
        } else {
            self.identity
                .require_authenticated(&self.operation.operation)?;
        }
        if let Some(token) = &self.bounds.cancellation {
            token.check_not_cancelled()?;
        }
        Ok(())
    }
}

/// Decode a request envelope — fail closed on unknown schema versions.
pub fn decode_remote_request_envelope_json(bytes: &[u8]) -> StorageResult<RemoteRequestEnvelope> {
    let value: RemoteRequestEnvelope = serde_json::from_slice(bytes).map_err(|err| {
        StorageError::invalid_request(format!("remote request envelope decode: {err}"))
    })?;
    value.validate()?;
    Ok(value)
}
