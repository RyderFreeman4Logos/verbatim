use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

const COLLECTION_WATCHERS_STATUS_ARTIFACT_ID: &str = "collections-watchers-status";

#[derive(Debug, Clone, Serialize)]
struct CollectionWatchersStatusResponseBody<'a> {
    watchers: &'a [super::CollectionWatcherStatus],
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectionWatchersStatusResponseWire {
    watchers: Vec<super::CollectionWatcherStatus>,
    identity: CanonicalIdentity,
}

fn collection_watchers_status_result_identity(
    watchers: &[super::CollectionWatcherStatus],
) -> Result<CanonicalIdentity> {
    CanonicalIdentity::from_body(
        WireArtifactKind::CollectionWatchersStatusResult,
        WIRE_SCHEMA_VERSION,
        COLLECTION_WATCHERS_STATUS_ARTIFACT_ID,
        &encode_wire_document(&CollectionWatchersStatusResponseBody { watchers })?,
    )
}

fn validate_collection_watchers_status_result_identity(
    response: &super::CollectionWatchersStatusResponse,
) -> Result<()> {
    response.identity.validate()?;
    if response.identity.artifact_id != COLLECTION_WATCHERS_STATUS_ARTIFACT_ID {
        anyhow::bail!("collection-watchers-status-result identity has an unexpected artifact ID");
    }
    let expected = collection_watchers_status_result_identity(&response.watchers)?;
    if response.identity != expected {
        anyhow::bail!(
            "collection-watchers-status-result identity does not match the collection watchers status response body"
        );
    }
    Ok(())
}

impl super::CollectionWatchersStatusResponse {
    pub fn new(watchers: Vec<super::CollectionWatcherStatus>) -> Result<Self> {
        let identity = collection_watchers_status_result_identity(&watchers)?;
        Ok(Self { watchers, identity })
    }
}

impl Serialize for super::CollectionWatchersStatusResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        validate_collection_watchers_status_result_identity(self)
            .map_err(serde::ser::Error::custom)?;
        CollectionWatchersStatusResponseWire {
            watchers: self.watchers.clone(),
            identity: self.identity.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for super::CollectionWatchersStatusResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = CollectionWatchersStatusResponseWire::deserialize(deserializer)?;
        let response = Self {
            watchers: wire.watchers,
            identity: wire.identity,
        };
        validate_collection_watchers_status_result_identity(&response)
            .map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}
