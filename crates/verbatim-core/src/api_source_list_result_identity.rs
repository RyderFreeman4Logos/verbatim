use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::api::SourceResponse;
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Clone, PartialEq)]
pub struct SourceListResponse {
    pub sources: Vec<SourceResponse>,
    pub identity: CanonicalIdentity,
}

#[derive(Debug, Serialize)]
struct SourceListResponseBody<'a> {
    sources: &'a [SourceResponse],
}

fn source_list_result_identity(sources: &[SourceResponse]) -> Result<CanonicalIdentity> {
    CanonicalIdentity::from_body(
        WireArtifactKind::SourceListResult,
        WIRE_SCHEMA_VERSION,
        "sources",
        &encode_wire_document(&SourceListResponseBody { sources })?,
    )
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceListResponseWire {
    sources: Vec<SourceResponse>,
    identity: CanonicalIdentity,
}

impl SourceListResponse {
    pub fn new(sources: Vec<SourceResponse>) -> Result<Self> {
        let identity = source_list_result_identity(&sources)?;
        Ok(Self { sources, identity })
    }

    fn stamp_identity(&self) -> Result<CanonicalIdentity> {
        let expected = source_list_result_identity(&self.sources)?;
        self.identity.validate()?;
        if self.identity != expected {
            anyhow::bail!("source-list identity does not match the source list response body");
        }
        Ok(expected)
    }
}

impl Serialize for SourceListResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        let identity = self.stamp_identity().map_err(serde::ser::Error::custom)?;
        SourceListResponseWire {
            sources: self.sources.clone(),
            identity,
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for SourceListResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = SourceListResponseWire::deserialize(deserializer)?;
        let response = Self {
            sources: wire.sources,
            identity: wire.identity,
        };
        response
            .stamp_identity()
            .map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}

#[cfg(test)]
#[path = "api_source_list_result_identity_wire_tests.rs"]
mod wire_tests;
