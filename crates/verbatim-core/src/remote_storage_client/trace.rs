//! Trace context propagation hooks for remote storage/index calls.

use serde::{Deserialize, Serialize};

use crate::observability_contract::TraceContext;
use crate::storage_ports::{StorageError, StorageResult};

/// How a client should treat inbound/outbound trace baggage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TracePropagationMode {
    /// Continue the parent span tree when span ids are present.
    ChildSpan,
    /// Queue/async hop: keep correlation IDs, clear span tree.
    AsyncLink,
    /// Do not emit remote span ids (still carry request_id).
    CorrelationOnly,
}

/// Carrier attached to remote requests — lightweight wrapper over
/// [`TraceContext`] plus propagation mode. No OTLP exporter.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RemoteTraceCarrier {
    pub mode: TracePropagationMode,
    pub context: TraceContext,
}

impl RemoteTraceCarrier {
    pub fn new(mode: TracePropagationMode, context: TraceContext) -> StorageResult<Self> {
        context.validate().map_err(|err| {
            StorageError::invalid_request(format!("remote trace context invalid: {err}"))
        })?;
        Ok(Self { mode, context })
    }

    pub fn from_request_id(
        request_id: impl Into<String>,
        mode: TracePropagationMode,
    ) -> StorageResult<Self> {
        let context = TraceContext::from_request_id(request_id).map_err(|err| {
            StorageError::invalid_request(format!("remote trace request_id: {err}"))
        })?;
        Self::new(mode, context)
    }

    pub fn validate(&self) -> StorageResult<()> {
        self.context.validate().map_err(|err| {
            StorageError::invalid_request(format!("remote trace context invalid: {err}"))
        })
    }

    /// Produce the outbound context according to [`TracePropagationMode`].
    pub fn outbound_context(&self) -> StorageResult<TraceContext> {
        self.validate()?;
        match self.mode {
            TracePropagationMode::ChildSpan => Ok(self.context.clone()),
            TracePropagationMode::AsyncLink => self.context.for_async_link().map_err(|err| {
                StorageError::invalid_request(format!("remote trace async link: {err}"))
            }),
            TracePropagationMode::CorrelationOnly => {
                let mut ctx = self.context.clone();
                ctx.span_id = None;
                ctx.parent_span_id = None;
                ctx.trace_id = None;
                ctx.validate().map_err(|err| {
                    StorageError::invalid_request(format!("remote correlation-only: {err}"))
                })?;
                Ok(ctx)
            }
        }
    }

    /// Optional request id for storage auth / envelope correlation.
    pub fn request_id(&self) -> &str {
        &self.context.request_id
    }
}
