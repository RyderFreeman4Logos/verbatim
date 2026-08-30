use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::api::{
    AppliedCollectionFilterResponse, CollectionFilterRequest, CollectionFilterResponse,
};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

const ASK_COLLECTION_FILTER_EVENT_ARTIFACT_ID: &str = "ask-stream-collection-filter";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AskCollectionFilterEvent {
    pub requested: CollectionFilterRequest,
    pub union_source_count: usize,
    pub applied: Vec<AppliedCollectionFilterResponse>,
    pub warnings: Vec<String>,
    pub stale: bool,
    pub identity: CanonicalIdentity,
}

#[derive(Debug, Serialize)]
struct AskCollectionFilterEventIdentityBody<'a> {
    requested: &'a CollectionFilterRequest,
    union_source_count: usize,
    applied: &'a [AppliedCollectionFilterResponse],
    warnings: &'a [String],
    stale: bool,
}

#[derive(Debug, Serialize, Deserialize)]
struct AskCollectionFilterEventWire {
    requested: CollectionFilterRequest,
    union_source_count: usize,
    applied: Vec<AppliedCollectionFilterResponse>,
    warnings: Vec<String>,
    stale: bool,
    identity: CanonicalIdentity,
}

fn stamp_ask_collection_filter_event_identity(
    body: &AskCollectionFilterEventIdentityBody<'_>,
    supplied: Option<&CanonicalIdentity>,
) -> Result<CanonicalIdentity> {
    let expected = CanonicalIdentity::from_body(
        WireArtifactKind::AskCollectionFilterEvent,
        WIRE_SCHEMA_VERSION,
        ASK_COLLECTION_FILTER_EVENT_ARTIFACT_ID,
        &encode_wire_document(body)?,
    )?;
    if let Some(supplied) = supplied {
        supplied.validate()?;
        if supplied != &expected {
            anyhow::bail!("ask-collection-filter-event identity does not match the filter body");
        }
    }
    Ok(expected)
}

impl AskCollectionFilterEvent {
    /// Creates a stream filter event with its canonical body identity bound.
    pub fn new(response: CollectionFilterResponse) -> Result<Self> {
        let CollectionFilterResponse {
            requested,
            union_source_count,
            applied,
            warnings,
            stale,
        } = response;
        let body = AskCollectionFilterEventIdentityBody {
            requested: &requested,
            union_source_count,
            applied: &applied,
            warnings: &warnings,
            stale,
        };
        let identity = stamp_ask_collection_filter_event_identity(&body, None)?;
        Ok(Self {
            requested,
            union_source_count,
            applied,
            warnings,
            stale,
            identity,
        })
    }
}

impl Serialize for AskCollectionFilterEvent {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let body = AskCollectionFilterEventIdentityBody {
            requested: &self.requested,
            union_source_count: self.union_source_count,
            applied: &self.applied,
            warnings: &self.warnings,
            stale: self.stale,
        };
        let identity = stamp_ask_collection_filter_event_identity(&body, Some(&self.identity))
            .map_err(serde::ser::Error::custom)?;
        AskCollectionFilterEventWire {
            requested: self.requested.clone(),
            union_source_count: self.union_source_count,
            applied: self.applied.clone(),
            warnings: self.warnings.clone(),
            stale: self.stale,
            identity,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for AskCollectionFilterEvent {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = AskCollectionFilterEventWire::deserialize(deserializer)?;
        let body = AskCollectionFilterEventIdentityBody {
            requested: &wire.requested,
            union_source_count: wire.union_source_count,
            applied: &wire.applied,
            warnings: &wire.warnings,
            stale: wire.stale,
        };
        let identity = stamp_ask_collection_filter_event_identity(&body, Some(&wire.identity))
            .map_err(serde::de::Error::custom)?;
        Ok(Self {
            requested: wire.requested,
            union_source_count: wire.union_source_count,
            applied: wire.applied,
            warnings: wire.warnings,
            stale: wire.stale,
            identity,
        })
    }
}

#[cfg(test)]
#[path = "api_ask_collection_filter_identity_wire_tests.rs"]
mod wire_tests;
