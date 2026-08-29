use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::api::{
    ChunkingProfileStatusResponse, EmbeddingCapabilityStatusResponse, IndexStatusResponse,
};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Serialize)]
struct IndexStatusResponseBody<'a> {
    embedding_enabled: bool,
    active_profile_id: &'a str,
    source_count: usize,
    stale_source_count: usize,
    stale_source_ids: &'a [String],
    capability: &'a EmbeddingCapabilityStatusResponse,
    chunking: &'a ChunkingProfileStatusResponse,
    messages: &'a [String],
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct IndexStatusResponseWire {
    embedding_enabled: bool,
    active_profile_id: String,
    source_count: usize,
    stale_source_count: usize,
    stale_source_ids: Vec<String>,
    capability: EmbeddingCapabilityStatusResponse,
    chunking: ChunkingProfileStatusResponse,
    #[serde(default)]
    messages: Vec<String>,
    identity: CanonicalIdentity,
}

#[allow(clippy::too_many_arguments)]
fn index_status_result_identity_from_parts(
    embedding_enabled: bool,
    active_profile_id: &str,
    source_count: usize,
    stale_source_count: usize,
    stale_source_ids: &[String],
    capability: &EmbeddingCapabilityStatusResponse,
    chunking: &ChunkingProfileStatusResponse,
    messages: &[String],
) -> Result<CanonicalIdentity> {
    CanonicalIdentity::from_body(
        WireArtifactKind::IndexStatusResult,
        WIRE_SCHEMA_VERSION,
        "index-status",
        &encode_wire_document(&IndexStatusResponseBody {
            embedding_enabled,
            active_profile_id,
            source_count,
            stale_source_count,
            stale_source_ids,
            capability,
            chunking,
            messages,
        })?,
    )
}

fn index_status_result_identity(response: &IndexStatusResponse) -> Result<CanonicalIdentity> {
    index_status_result_identity_from_parts(
        response.embedding_enabled,
        &response.active_profile_id,
        response.source_count,
        response.stale_source_count,
        &response.stale_source_ids,
        &response.capability,
        &response.chunking,
        &response.messages,
    )
}

fn validate_index_status_result_identity(response: &IndexStatusResponse) -> Result<()> {
    response.identity.validate()?;
    let expected = index_status_result_identity(response)?;
    if response.identity != expected {
        anyhow::bail!("index-status-result identity does not match the index status response body");
    }
    Ok(())
}

impl IndexStatusResponse {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        embedding_enabled: bool,
        active_profile_id: String,
        source_count: usize,
        stale_source_count: usize,
        stale_source_ids: Vec<String>,
        capability: EmbeddingCapabilityStatusResponse,
        chunking: ChunkingProfileStatusResponse,
        messages: Vec<String>,
    ) -> Result<Self> {
        let identity = index_status_result_identity_from_parts(
            embedding_enabled,
            &active_profile_id,
            source_count,
            stale_source_count,
            &stale_source_ids,
            &capability,
            &chunking,
            &messages,
        )?;
        Ok(Self {
            embedding_enabled,
            active_profile_id,
            source_count,
            stale_source_count,
            stale_source_ids,
            capability,
            chunking,
            messages,
            identity,
        })
    }
}

impl Serialize for IndexStatusResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        validate_index_status_result_identity(self).map_err(serde::ser::Error::custom)?;
        IndexStatusResponseWire {
            embedding_enabled: self.embedding_enabled,
            active_profile_id: self.active_profile_id.clone(),
            source_count: self.source_count,
            stale_source_count: self.stale_source_count,
            stale_source_ids: self.stale_source_ids.clone(),
            capability: self.capability.clone(),
            chunking: self.chunking.clone(),
            messages: self.messages.clone(),
            identity: self.identity.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for IndexStatusResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = IndexStatusResponseWire::deserialize(deserializer)?;
        let response = Self {
            embedding_enabled: wire.embedding_enabled,
            active_profile_id: wire.active_profile_id,
            source_count: wire.source_count,
            stale_source_count: wire.stale_source_count,
            stale_source_ids: wire.stale_source_ids,
            capability: wire.capability,
            chunking: wire.chunking,
            messages: wire.messages,
            identity: wire.identity,
        };
        validate_index_status_result_identity(&response).map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}

#[cfg(test)]
#[path = "api_index_status_result_identity_wire_tests.rs"]
mod api_index_status_result_identity_wire_tests;
