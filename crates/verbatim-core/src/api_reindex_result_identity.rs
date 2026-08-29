use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Clone, Serialize)]
struct ReindexResponseBody {
    reindexed: usize,
}

#[derive(Debug, Serialize, Deserialize)]
struct ReindexResponseWire {
    reindexed: usize,
    identity: CanonicalIdentity,
}

fn reindex_result_identity(reindexed: usize) -> Result<CanonicalIdentity> {
    let body = ReindexResponseBody { reindexed };
    CanonicalIdentity::from_body(
        WireArtifactKind::ReindexResult,
        WIRE_SCHEMA_VERSION,
        "reindex-result",
        &encode_wire_document(&body)?,
    )
}

fn expected_reindex_result_identity(
    response: &super::ReindexResponse,
) -> Result<CanonicalIdentity> {
    reindex_result_identity(response.reindexed)
}

fn validate_reindex_result_identity(response: &super::ReindexResponse) -> Result<()> {
    response.identity.validate()?;
    let expected = expected_reindex_result_identity(response)?;
    if response.identity != expected {
        anyhow::bail!("reindex-result identity does not match the reindex response body");
    }
    Ok(())
}

impl super::ReindexResponse {
    pub fn new(reindexed: usize) -> Result<Self> {
        Ok(Self {
            reindexed,
            identity: reindex_result_identity(reindexed)?,
        })
    }
}

impl Serialize for super::ReindexResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        validate_reindex_result_identity(self).map_err(serde::ser::Error::custom)?;
        ReindexResponseWire {
            reindexed: self.reindexed,
            identity: self.identity.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for super::ReindexResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let response = ReindexResponseWire::deserialize(deserializer)?;
        let response = Self {
            reindexed: response.reindexed,
            identity: response.identity,
        };
        validate_reindex_result_identity(&response).map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}
