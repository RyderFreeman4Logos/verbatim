//! Trace correlation context for cross-service request/job hops.

use anyhow::Result;
use serde::{Deserialize, Serialize};

use super::common::require_non_empty;
use super::common::validate_schema_version;
use super::OBSERVABILITY_CONTRACT_SCHEMA_VERSION;

/// Correlated IDs that must travel with every request/job hop.
///
/// Fields are optional at construction so partial contexts (e.g. early client
/// ingress with only `request_id`) remain representable.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct TraceContext {
    /// Wire schema version. Must equal [`OBSERVABILITY_CONTRACT_SCHEMA_VERSION`].
    pub schema_version: u32,
    /// End-to-end request identity (client → coordinator → answer).
    pub request_id: String,
    /// One retrieval run under a request (may fan out to multiple retrievers).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub retrieval_run_id: Option<String>,
    /// ContextPack identity produced for grounding/generation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_pack_id: Option<String>,
    /// Workflow / GraphRAG / multi-step job run identity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workflow_run_id: Option<String>,
    /// Daemon task identity when the work is scheduled as a task.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task_id: Option<String>,
    /// Publication / index generation fence for freshness correlation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub publication_generation: Option<String>,
    /// W3C-style trace id (32 hex chars preferred; not enforced as hex here).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trace_id: Option<String>,
    /// Current span id within the trace (16 hex chars preferred).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub span_id: Option<String>,
    /// Parent span id when continuing a tree (absent for root).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parent_span_id: Option<String>,
}

/// Field bundle for [`TraceContext::new`].
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct TraceContextFields {
    pub request_id: String,
    pub retrieval_run_id: Option<String>,
    pub context_pack_id: Option<String>,
    pub workflow_run_id: Option<String>,
    pub task_id: Option<String>,
    pub publication_generation: Option<String>,
    pub trace_id: Option<String>,
    pub span_id: Option<String>,
    pub parent_span_id: Option<String>,
}

impl TraceContext {
    /// Build a current-schema context. `request_id` must be non-empty.
    pub fn new(fields: TraceContextFields) -> Result<Self> {
        let ctx = Self {
            schema_version: OBSERVABILITY_CONTRACT_SCHEMA_VERSION,
            request_id: fields.request_id,
            retrieval_run_id: fields.retrieval_run_id,
            context_pack_id: fields.context_pack_id,
            workflow_run_id: fields.workflow_run_id,
            task_id: fields.task_id,
            publication_generation: fields.publication_generation,
            trace_id: fields.trace_id,
            span_id: fields.span_id,
            parent_span_id: fields.parent_span_id,
        };
        ctx.validate()?;
        Ok(ctx)
    }

    /// Minimal ingress context from a non-empty request id.
    pub fn from_request_id(request_id: impl Into<String>) -> Result<Self> {
        Self::new(TraceContextFields {
            request_id: request_id.into(),
            retrieval_run_id: None,
            context_pack_id: None,
            workflow_run_id: None,
            task_id: None,
            publication_generation: None,
            trace_id: None,
            span_id: None,
            parent_span_id: None,
        })
    }

    pub fn validate_schema(&self) -> Result<()> {
        validate_schema_version(self.schema_version)
    }

    /// Reject empty required correlation fields and unknown schema versions.
    pub fn validate(&self) -> Result<()> {
        self.validate_schema()?;
        require_non_empty("trace context request_id", &self.request_id)?;
        Ok(())
    }

    /// Child context continuing the same request/trace under a new span id.
    pub fn child_span(&self, span_id: impl Into<String>) -> Result<Self> {
        self.validate()?;
        let span_id = span_id.into();
        require_non_empty("child span_id", &span_id)?;
        Self::new(TraceContextFields {
            request_id: self.request_id.clone(),
            retrieval_run_id: self.retrieval_run_id.clone(),
            context_pack_id: self.context_pack_id.clone(),
            workflow_run_id: self.workflow_run_id.clone(),
            task_id: self.task_id.clone(),
            publication_generation: self.publication_generation.clone(),
            trace_id: self.trace_id.clone(),
            span_id: Some(span_id),
            parent_span_id: self.span_id.clone(),
        })
    }

    /// Propagate across a queue/async boundary without trusting remote baggage.
    ///
    /// Only well-known correlation IDs are carried. Span tree fields are cleared
    /// so the consumer opens a linked root under the same request/trace.
    pub fn for_async_link(&self) -> Result<Self> {
        self.validate()?;
        Self::new(TraceContextFields {
            request_id: self.request_id.clone(),
            retrieval_run_id: self.retrieval_run_id.clone(),
            context_pack_id: self.context_pack_id.clone(),
            workflow_run_id: self.workflow_run_id.clone(),
            task_id: self.task_id.clone(),
            publication_generation: self.publication_generation.clone(),
            trace_id: self.trace_id.clone(),
            span_id: None,
            parent_span_id: None,
        })
    }

    /// True when both contexts share the same request (primary correlation key).
    pub fn same_request_as(&self, other: &Self) -> bool {
        self.request_id == other.request_id
    }
}

/// Decode JSON — fail closed on unknown schema versions.
pub fn decode_trace_context_json(bytes: &[u8]) -> Result<TraceContext> {
    let ctx: TraceContext = serde_json::from_slice(bytes)?;
    ctx.validate()?;
    Ok(ctx)
}
