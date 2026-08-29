use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::collection::{validate_collection_name, CollectionSyncReport};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Clone, Serialize)]
struct CollectionSyncResponseBody<'a> {
    report: &'a CollectionSyncReport,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectionSyncResponseWire {
    report: CollectionSyncReport,
    identity: CanonicalIdentity,
}

fn collection_sync_result_identity(
    collection_name: &str,
    report: &CollectionSyncReport,
) -> Result<CanonicalIdentity> {
    validate_collection_name(collection_name)?;
    CanonicalIdentity::from_body(
        WireArtifactKind::CollectionSyncResult,
        WIRE_SCHEMA_VERSION,
        collection_name,
        &encode_wire_document(&CollectionSyncResponseBody { report })?,
    )
}

fn validate_collection_sync_result_identity(
    response: &super::CollectionSyncResponse,
) -> Result<()> {
    validate_collection_name(&response.identity.artifact_id)?;
    response.identity.validate()?;
    let expected =
        collection_sync_result_identity(&response.identity.artifact_id, &response.report)?;
    if response.identity != expected {
        anyhow::bail!(
            "collection-sync-result identity does not match the collection sync response body"
        );
    }
    Ok(())
}

impl super::CollectionSyncResponse {
    pub fn new(collection_name: impl Into<String>, report: CollectionSyncReport) -> Result<Self> {
        let collection_name = collection_name.into();
        let identity = collection_sync_result_identity(&collection_name, &report)?;
        Ok(Self { report, identity })
    }

    pub fn validate_for_collection(&self, collection_name: &str) -> Result<()> {
        validate_collection_name(collection_name)?;
        if self.identity.artifact_id != collection_name {
            anyhow::bail!(
                "collection-sync-result identity does not match the requested collection"
            );
        }
        validate_collection_sync_result_identity(self)
    }
}

impl Serialize for super::CollectionSyncResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        validate_collection_sync_result_identity(self).map_err(serde::ser::Error::custom)?;
        CollectionSyncResponseWire {
            report: self.report.clone(),
            identity: self.identity.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for super::CollectionSyncResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = CollectionSyncResponseWire::deserialize(deserializer)?;
        let response = Self {
            report: wire.report,
            identity: wire.identity,
        };
        validate_collection_sync_result_identity(&response).map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}
