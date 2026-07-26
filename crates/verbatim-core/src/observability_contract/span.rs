//! Bounded span specifications and async links.

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::time::Duration;

use super::common::{
    ensure_end_after_start, require_error_class, require_non_empty, validate_schema_version,
};
use super::trace::TraceContext;
use super::OBSERVABILITY_CONTRACT_SCHEMA_VERSION;

/// Maximum attribute entries allowed on a single span (hard bound).
pub const MAX_SPAN_ATTRIBUTES: usize = 32;

/// Maximum link entries allowed on a single span (hard bound).
pub const MAX_SPAN_LINKS: usize = 16;

/// Span status for bounded stage instrumentation.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SpanStatus {
    Unset,
    Ok,
    Error,
}

/// Lightweight link to another span/trace (queues, fan-out/fan-in).
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct SpanLink {
    pub trace_id: String,
    pub span_id: String,
    /// Optional relationship label (`follows_from`, `child_of`, …).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relationship: Option<String>,
}

impl SpanLink {
    pub fn new(trace_id: impl Into<String>, span_id: impl Into<String>) -> Result<Self> {
        let link = Self {
            trace_id: trace_id.into(),
            span_id: span_id.into(),
            relationship: None,
        };
        link.validate()?;
        Ok(link)
    }

    pub fn with_relationship(mut self, relationship: impl Into<String>) -> Self {
        self.relationship = Some(relationship.into());
        self
    }

    pub fn validate(&self) -> Result<()> {
        require_non_empty("span link trace_id", &self.trace_id)?;
        require_non_empty("span link span_id", &self.span_id)?;
        Ok(())
    }
}

/// Bounded span specification (not a live OTel span).
///
/// Attributes are privacy-reviewed string pairs. Callers must run sensitive
/// values through [`crate::observability_contract::RedactionPolicy`] before attach.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SpanSpec {
    pub schema_version: u32,
    pub name: String,
    /// Owning correlation context at span open.
    pub context: TraceContext,
    /// Unix epoch milliseconds (UTC) when the span started.
    pub start_unix_ms: u64,
    /// Unix epoch milliseconds (UTC) when the span ended; `None` while open.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub end_unix_ms: Option<u64>,
    pub status: SpanStatus,
    /// Stable error class when `status == Error` (never raw exception text).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_class: Option<String>,
    /// Bounded, low-cardinality attributes (BTreeMap for stable serde).
    #[serde(default)]
    pub attributes: BTreeMap<String, String>,
    /// Links to asynchronous / fan-in peers.
    #[serde(default)]
    pub links: Vec<SpanLink>,
}

/// Constructor inputs for [`SpanSpec::open`].
#[derive(Debug, Clone)]
pub struct SpanOpenParams {
    pub name: String,
    pub context: TraceContext,
    pub start_unix_ms: u64,
}

impl SpanSpec {
    /// Open a span in `Unset` status with empty attributes/links.
    pub fn open(params: SpanOpenParams) -> Result<Self> {
        params.context.validate()?;
        require_non_empty("span name", &params.name)?;
        Ok(Self {
            schema_version: OBSERVABILITY_CONTRACT_SCHEMA_VERSION,
            name: params.name,
            context: params.context,
            start_unix_ms: params.start_unix_ms,
            end_unix_ms: None,
            status: SpanStatus::Unset,
            error_class: None,
            attributes: BTreeMap::new(),
            links: Vec::new(),
        })
    }

    pub fn validate_schema(&self) -> Result<()> {
        validate_schema_version(self.schema_version)
    }

    pub fn validate(&self) -> Result<()> {
        self.validate_schema()?;
        require_non_empty("span name", &self.name)?;
        self.context.validate()?;
        if self.attributes.len() > MAX_SPAN_ATTRIBUTES {
            bail!(
                "span attributes exceed hard bound {MAX_SPAN_ATTRIBUTES} (got {})",
                self.attributes.len()
            );
        }
        if self.links.len() > MAX_SPAN_LINKS {
            bail!(
                "span links exceed hard bound {MAX_SPAN_LINKS} (got {})",
                self.links.len()
            );
        }
        for link in &self.links {
            link.validate()?;
        }
        if let Some(end) = self.end_unix_ms {
            ensure_end_after_start(self.start_unix_ms, end)?;
        }
        if matches!(self.status, SpanStatus::Error) {
            require_error_class(self.error_class.as_deref())?;
        }
        Ok(())
    }

    /// Attach a privacy-reviewed attribute. Rejects over-bound maps.
    pub fn set_attribute(
        &mut self,
        key: impl Into<String>,
        value: impl Into<String>,
    ) -> Result<()> {
        let key = key.into();
        require_non_empty("span attribute key", &key)?;
        if !self.attributes.contains_key(&key) && self.attributes.len() >= MAX_SPAN_ATTRIBUTES {
            bail!("span attributes exceed hard bound {MAX_SPAN_ATTRIBUTES}");
        }
        self.attributes.insert(key, value.into());
        Ok(())
    }

    /// Link an asynchronous peer span (queue hop, fan-out).
    pub fn add_link(&mut self, link: SpanLink) -> Result<()> {
        link.validate()?;
        if self.links.len() >= MAX_SPAN_LINKS {
            bail!("span links exceed hard bound {MAX_SPAN_LINKS}");
        }
        self.links.push(link);
        Ok(())
    }

    /// Close successfully with an end timestamp.
    pub fn end_ok(&mut self, end_unix_ms: u64) -> Result<()> {
        self.finish(end_unix_ms, SpanStatus::Ok, None)
    }

    /// Close with a stable error class (not a free-form message).
    pub fn end_error(&mut self, end_unix_ms: u64, error_class: impl Into<String>) -> Result<()> {
        self.finish(end_unix_ms, SpanStatus::Error, Some(error_class.into()))
    }

    fn finish(
        &mut self,
        end_unix_ms: u64,
        status: SpanStatus,
        error_class: Option<String>,
    ) -> Result<()> {
        ensure_end_after_start(self.start_unix_ms, end_unix_ms)?;
        if matches!(status, SpanStatus::Error) {
            require_error_class(error_class.as_deref())?;
        }
        self.end_unix_ms = Some(end_unix_ms);
        self.status = status;
        self.error_class = error_class;
        self.validate()
    }

    /// Duration when the span has ended.
    pub fn duration(&self) -> Option<Duration> {
        let end = self.end_unix_ms?;
        Some(Duration::from_millis(
            end.saturating_sub(self.start_unix_ms),
        ))
    }
}

/// Decode JSON — fail closed on unknown schema versions.
pub fn decode_span_spec_json(bytes: &[u8]) -> Result<SpanSpec> {
    let span: SpanSpec = serde_json::from_slice(bytes)?;
    span.validate()?;
    Ok(span)
}
