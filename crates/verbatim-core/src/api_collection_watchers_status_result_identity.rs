use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::collection::{validate_collection_name, CollectionRecord};

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

#[derive(Debug, Clone, Serialize)]
struct CollectionWatcherResponseBody<'a> {
    collection: &'a CollectionRecord,
    watcher: &'a super::CollectionWatcherStatus,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectionWatcherResponseWire {
    collection: CollectionRecord,
    watcher: super::CollectionWatcherStatus,
    identity: CanonicalIdentity,
}

fn collection_watcher_result_identity(
    collection_name: &str,
    collection: &CollectionRecord,
    watcher: &super::CollectionWatcherStatus,
) -> Result<CanonicalIdentity> {
    validate_collection_name(collection_name)?;
    if collection.name != collection_name || watcher.collection_name != collection_name {
        anyhow::bail!("collection-watcher-result name does not match the response collection");
    }
    CanonicalIdentity::from_body(
        WireArtifactKind::CollectionWatcherResult,
        WIRE_SCHEMA_VERSION,
        collection_name,
        &encode_wire_document(&CollectionWatcherResponseBody {
            collection,
            watcher,
        })?,
    )
}

fn validate_collection_watcher_result_identity(
    response: &super::CollectionWatcherResponse,
) -> Result<()> {
    validate_collection_name(&response.identity.artifact_id)?;
    if response.identity.artifact_id != response.collection.name
        || response.identity.artifact_id != response.watcher.collection_name
    {
        anyhow::bail!("collection-watcher-result identity does not match the response collection");
    }
    response.identity.validate()?;
    let expected = collection_watcher_result_identity(
        &response.identity.artifact_id,
        &response.collection,
        &response.watcher,
    )?;
    if response.identity != expected {
        anyhow::bail!(
            "collection-watcher-result identity does not match the collection watcher response body"
        );
    }
    Ok(())
}

impl super::CollectionWatcherResponse {
    pub fn new(
        collection_name: impl Into<String>,
        collection: CollectionRecord,
        watcher: super::CollectionWatcherStatus,
    ) -> Result<Self> {
        let collection_name = collection_name.into();
        let identity = collection_watcher_result_identity(&collection_name, &collection, &watcher)?;
        Ok(Self {
            collection,
            watcher,
            identity,
        })
    }

    pub fn validate_for_collection(&self, collection_name: &str) -> Result<()> {
        validate_collection_name(collection_name)?;
        if self.identity.artifact_id != collection_name {
            anyhow::bail!(
                "collection-watcher-result identity does not match the requested collection"
            );
        }
        validate_collection_watcher_result_identity(self)
    }
}

impl Serialize for super::CollectionWatcherResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        validate_collection_watcher_result_identity(self).map_err(serde::ser::Error::custom)?;
        CollectionWatcherResponseWire {
            collection: self.collection.clone(),
            watcher: self.watcher.clone(),
            identity: self.identity.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for super::CollectionWatcherResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = CollectionWatcherResponseWire::deserialize(deserializer)?;
        let response = Self {
            collection: wire.collection,
            watcher: wire.watcher,
            identity: wire.identity,
        };
        validate_collection_watcher_result_identity(&response).map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}

#[cfg(test)]
mod collection_watcher_result_identity_wire_tests {
    include!("api_collection_watcher_result_identity_wire_tests.rs");
}
