use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::collection::{validate_collection_name, CollectionStatus};
use crate::wire_schemas::{
    encode_wire_document, CanonicalIdentity, WireArtifactKind, WIRE_SCHEMA_VERSION,
};

#[derive(Debug, Clone, Serialize)]
struct CollectionStatusResponseBody<'a> {
    status: &'a CollectionStatus,
}

#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CollectionStatusResponseWire {
    status: CollectionStatus,
    identity: CanonicalIdentity,
}

fn collection_status_result_identity(
    collection_name: &str,
    status: &CollectionStatus,
) -> Result<CanonicalIdentity> {
    validate_collection_name(collection_name)?;
    if status.collection.name != collection_name {
        anyhow::bail!("collection-status-result name does not match the status collection name");
    }
    CanonicalIdentity::from_body(
        WireArtifactKind::CollectionStatusResult,
        WIRE_SCHEMA_VERSION,
        collection_name,
        &encode_wire_document(&CollectionStatusResponseBody { status })?,
    )
}

fn validate_collection_status_result_identity(
    response: &super::CollectionStatusResponse,
) -> Result<()> {
    validate_collection_name(&response.identity.artifact_id)?;
    if response.identity.artifact_id != response.status.collection.name {
        anyhow::bail!(
            "collection-status-result identity does not match the status collection name"
        );
    }
    response.identity.validate()?;
    let expected =
        collection_status_result_identity(&response.identity.artifact_id, &response.status)?;
    if response.identity != expected {
        anyhow::bail!(
            "collection-status-result identity does not match the collection status response body"
        );
    }
    Ok(())
}

impl super::CollectionStatusResponse {
    pub fn new(collection_name: impl Into<String>, status: CollectionStatus) -> Result<Self> {
        let collection_name = collection_name.into();
        let identity = collection_status_result_identity(&collection_name, &status)?;
        Ok(Self { status, identity })
    }

    pub fn validate_for_collection(&self, collection_name: &str) -> Result<()> {
        validate_collection_name(collection_name)?;
        if self.identity.artifact_id != collection_name {
            anyhow::bail!(
                "collection-status-result identity does not match the requested collection"
            );
        }
        validate_collection_status_result_identity(self)
    }
}

impl Serialize for super::CollectionStatusResponse {
    fn serialize<S>(&self, serializer: S) -> std::result::Result<S::Ok, S::Error>
    where
        S: serde::Serializer,
    {
        validate_collection_status_result_identity(self).map_err(serde::ser::Error::custom)?;
        CollectionStatusResponseWire {
            status: self.status.clone(),
            identity: self.identity.clone(),
        }
        .serialize(serializer)
    }
}

impl<'de> Deserialize<'de> for super::CollectionStatusResponse {
    fn deserialize<D>(deserializer: D) -> std::result::Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let wire = CollectionStatusResponseWire::deserialize(deserializer)?;
        let response = Self {
            status: wire.status,
            identity: wire.identity,
        };
        validate_collection_status_result_identity(&response).map_err(serde::de::Error::custom)?;
        Ok(response)
    }
}
